use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::meta::ParseNestedMeta;

use super::fields::DefaultKind;
use super::types::{parse_byte_unit, strip_raw_prefix};
use crate::parser::{SerdeSide, extract_default, extract_field_rename_for, rename_field};

/// Field-level `#[form_data(...)]` keys, read in ONE pass over every key so
/// an unrelated `key = value` before the one asked for never hides it.
#[derive(Default)]
pub(super) struct FormDataAttrs {
    /// `field_name = "..."` (a non-string value is a spanned error).
    pub(super) field_name: Option<String>,
    /// `limit = "..."`, validated: `"unlimited"` or a byte size.
    pub(super) limit: Option<syn::LitStr>,
    /// `default` / `default = true` (`default = false` is no default).
    pub(super) default: bool,
}

/// Consume the `= value` / `(...)` payload of a key the caller does not read,
/// so the nested-meta walk continues with the next key.
fn skip_value(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        meta.input.parse::<proc_macro2::Group>()?;
    }
    Ok(())
}

/// A flag key's value, as `axum_typed_multipart` reads it: a bare `key` /
/// `key = true` is `true`, `key = false` is `false`.
fn flag_value(meta: &ParseNestedMeta<'_>) -> syn::Result<bool> {
    if meta.input.peek(syn::Token![=]) {
        return Ok(meta.value()?.parse::<syn::LitBool>()?.value);
    }
    skip_value(meta)?;
    Ok(true)
}

/// The `= <expr>` payload of a key (`None` for a bare key).
fn value_expr(meta: &ParseNestedMeta<'_>) -> syn::Result<Option<syn::Expr>> {
    if meta.input.peek(syn::Token![=]) {
        return meta.value()?.parse().map(Some);
    }
    skip_value(meta)?;
    Ok(None)
}

/// The string literal of an expression, if it is one.
fn as_lit_str(expr: Option<&syn::Expr>) -> Option<&syn::LitStr> {
    match expr {
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        })) => Some(s),
        _ => None,
    }
}

/// Parse every `#[form_data(...)]` on a field. The first occurrence of a key
/// wins; malformed syntax, a non-string `field_name` and an invalid `limit`
/// are spanned errors.
pub(super) fn parse_form_data(attrs: &[syn::Attribute]) -> syn::Result<FormDataAttrs> {
    let mut out = FormDataAttrs::default();
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("form_data"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("field_name") {
                let expr = value_expr(&meta)?;
                // A non-string name (`field_name = upload_name`) would read the
                // part under the serde name, silently: it is an error instead.
                let Some(lit) = as_lit_str(expr.as_ref()) else {
                    let span_tokens =
                        expr.map_or_else(|| meta.path.to_token_stream(), |e| e.to_token_stream());
                    return Err(syn::Error::new_spanned(
                        span_tokens,
                        "expected `field_name = \"<name>\"`, e.g. `field_name = \"upload\"`",
                    ));
                };
                if out.field_name.is_none() {
                    out.field_name = Some(lit.value());
                }
            } else if meta.path.is_ident("limit") {
                let expr = value_expr(&meta)?;
                let Some(lit) = as_lit_str(expr.as_ref()) else {
                    let span_tokens =
                        expr.map_or_else(|| meta.path.to_token_stream(), |e| e.to_token_stream());
                    return Err(syn::Error::new_spanned(
                        span_tokens,
                        "expected `limit = \"<size>\"`, e.g. `limit = \"10MiB\"` or `limit = \"unlimited\"`",
                    ));
                };
                // Every literal is validated, not only the winning one: a
                // later invalid `limit` used to be silently ignored.
                limit_tokens(lit)?;
                if out.limit.is_none() {
                    out.limit = Some(lit.clone());
                }
            } else if meta.path.is_ident("default") {
                out.default |= flag_value(&meta)?;
            } else {
                skip_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// A field's bare `#[form_data(default)]`: the Multipart derive fills a
/// missing part with `Default::default()`, so the request schema must not
/// list the field as `required` (malformed attributes read as `false`; the
/// derive reports them).
pub fn has_form_data_default(attrs: &[syn::Attribute]) -> bool {
    parse_form_data(attrs).is_ok_and(|form| form.default)
}

/// Resolve the multipart field name using serde + form_data attributes.
///
/// Priority:
/// 1. `#[form_data(field_name = "...")]` (`form_field_name`)
/// 2. `#[serde(rename = "...")]` (the deserialize side: the derive parses)
/// 3. struct-level `rename_all` (deserialize side) applied to Rust field name
/// 4. Rust field name as-is
pub(super) fn resolve_field_name(
    ident: &syn::Ident,
    attrs: &[syn::Attribute],
    form_field_name: Option<&str>,
    rename_all: Option<&str>,
) -> String {
    if let Some(name) = form_field_name {
        return name.to_owned();
    }
    if let Some(name) = extract_field_rename_for(attrs, SerdeSide::Deserialize) {
        return name;
    }
    let rust_name = strip_raw_prefix(&ident.to_string());
    rename_field(&rust_name, rename_all)
}

/// Extract the `strict` flag from `#[try_from_multipart(strict)]`, reading
/// past any other key (`rename_all = "..."`) in the same list.
pub(super) fn extract_strict(attrs: &[syn::Attribute]) -> syn::Result<bool> {
    let mut strict = false;
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("try_from_multipart"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("strict") {
                strict |= flag_value(&meta)?;
                return Ok(());
            }
            skip_value(&meta)
        })?;
    }
    Ok(strict)
}

/// The `Option<usize>` tokens of a field's limit: `None` without one,
/// `usize::MAX` for `"unlimited"`, the byte count otherwise.
pub(super) fn extract_limit_tokens(limit: Option<&syn::LitStr>) -> TokenStream {
    // `parse_form_data` validated the literal already.
    limit.map_or_else(
        || quote! { std::option::Option::None },
        |lit| limit_tokens(lit).unwrap_or_else(syn::Error::into_compile_error),
    )
}

/// Validate one `limit` literal into its `Option::Some(..)` tokens.
fn limit_tokens(lit: &syn::LitStr) -> syn::Result<TokenStream> {
    let s = lit.value();
    if s == "unlimited" {
        // `usize::MAX` is the explicit unbounded sentinel: every
        // limit check (`total > limit`) is byte-for-byte
        // equivalent to the former `None` (never triggers), but
        // it is DISTINGUISHABLE from an ABSENT attribute (which
        // stays `None`).  That lets the runtime apply a
        // default cap to unannotated text fields (`String`) while
        // an explicit `limit = "unlimited"` opt-out stays
        // genuinely unbounded.
        return Ok(quote! { std::option::Option::Some(usize::MAX) });
    }
    let bytes = parse_byte_unit(&s).map_err(|msg| syn::Error::new(lit.span(), msg))?;
    Ok(quote! { std::option::Option::Some(#bytes) })
}

/// Resolve the default behavior for a field.
///
/// Priority:
/// 1. `#[form_data(default)]` — explicit form_data override (bare default)
/// 2. `#[serde(default)]` — bare default via `Default::default()`
/// 3. `#[serde(default = "fn_path")]` — custom default function
/// 4. Struct-level `#[serde(default)]` / `default = "fn"` — the field of the
///    container's default value (serde's rule: `<Self as Default>::default()`
///    / `fn()`, not the field type's `Default`)
/// 5. No default — field is required
pub(super) fn resolve_default_kind(
    attrs: &[syn::Attribute],
    form_default: bool,
    struct_default: bool,
) -> DefaultKind {
    if form_default {
        return DefaultKind::Trait;
    }
    if let Some(serde_default) = extract_default(attrs) {
        return serde_default.map_or(DefaultKind::Trait, DefaultKind::Function);
    }
    if struct_default {
        return DefaultKind::Container;
    }
    DefaultKind::None
}

/// The struct-level `#[serde(default)]` (`Trait`) / `#[serde(default =
/// "fn")]` (`Function`), or `None`.
pub(super) fn extract_struct_default(attrs: &[syn::Attribute]) -> DefaultKind {
    match extract_default(attrs) {
        Some(Some(path)) => DefaultKind::Function(path),
        Some(None) => DefaultKind::Trait,
        None => DefaultKind::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use syn::Fields;

    fn parse_field(code: &str) -> syn::Field {
        let input: syn::DeriveInput = syn::parse_str(&format!("struct T {{ {code} }}")).unwrap();
        match &input.data {
            syn::Data::Struct(s) => match &s.fields {
                Fields::Named(n) => n.named.first().unwrap().clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    }

    fn parse_attrs(code: &str) -> Vec<syn::Attribute> {
        parse_field(code).attrs
    }

    fn parse_struct_attrs(code: &str) -> Vec<syn::Attribute> {
        let input: syn::DeriveInput = syn::parse_str(code).unwrap();
        input.attrs
    }

    fn form_data(code: &str) -> FormDataAttrs {
        parse_form_data(&parse_attrs(code)).unwrap()
    }

    fn resolve(code: &str, rename_all: Option<&str>) -> String {
        let field = parse_field(code);
        let form = parse_form_data(&field.attrs).unwrap();
        resolve_field_name(
            field.ident.as_ref().unwrap(),
            &field.attrs,
            form.field_name.as_deref(),
            rename_all,
        )
    }

    #[rstest]
    #[case::present(r#"#[form_data(field_name = "custom")] pub x: String"#, Some("custom"))]
    #[case::absent("pub x: String", None)]
    #[case::other_key(r#"#[form_data(limit = "100")] pub x: String"#, None)]
    #[case::after_other_key(
        r#"#[form_data(limit = "100", default, field_name = "late")] pub x: String"#,
        Some("late")
    )]
    #[case::first_attribute_wins(
        r#"#[form_data(field_name = "a")] #[form_data(field_name = "b")] pub x: String"#,
        Some("a")
    )]
    #[case::unknown_list_key(
        r#"#[form_data(other(a = 1), field_name = "y")] pub x: String"#,
        Some("y")
    )]
    fn field_name_is_read_in_any_position(#[case] code: &str, #[case] expected: Option<&str>) {
        assert_eq!(form_data(code).field_name.as_deref(), expected);
    }

    /// R24-SM0-L4: a non-string or valueless `field_name` is a spanned error
    /// (it used to be ignored, reading the part under the serde name).
    #[rstest]
    #[case::non_string(r"#[form_data(field_name = 42)] pub x: String")]
    #[case::path_value(r"#[form_data(field_name = upload_name)] pub x: String")]
    #[case::valueless(r"#[form_data(field_name)] pub x: String")]
    fn malformed_field_name_is_a_spanned_error(#[case] code: &str) {
        let err = parse_form_data(&parse_attrs(code))
            .err()
            .expect("a non-string field_name is an error");
        assert!(err.to_string().contains("field_name"), "{err}");
    }

    #[rstest]
    #[case::plain("pub my_field: String", None, "my_field")]
    #[case::rename_all("pub my_field: String", Some("camelCase"), "myField")]
    #[case::serde_rename(
        r#"#[serde(rename = "custom")] pub my_field: String"#,
        Some("camelCase"),
        "custom"
    )]
    #[case::form_data_over_serde(
        r#"#[form_data(field_name = "override")] #[serde(rename = "serde_name")] pub my_field: String"#,
        Some("camelCase"),
        "override"
    )]
    // The derive PARSES a form, so serde's deserialize-side names apply —
    // the same names the request-view (`{Name}Input`) schema documents.
    #[case::sided_rename(
        r#"#[serde(rename(serialize = "out", deserialize = "in"))] pub my_field: String"#,
        None,
        "in"
    )]
    #[case::serialize_only_rename(
        r#"#[serde(rename(serialize = "out"))] pub my_field: String"#,
        None,
        "my_field"
    )]
    #[case::form_data_over_sided_rename(
        r#"#[serde(rename(deserialize = "in"))] #[form_data(field_name = "form")] pub my_field: String"#,
        None,
        "form"
    )]
    fn test_resolve_field_name(
        #[case] src: &str,
        #[case] rename_all: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(resolve(src, rename_all), expected);
    }

    #[rstest]
    #[case::present("#[try_from_multipart(strict)] struct T { }", true)]
    #[case::absent("struct T { }", false)]
    #[case::other_key(
        "#[try_from_multipart(rename_all = \"camelCase\")] struct T { }",
        false
    )]
    #[case::after_other_key(
        "#[try_from_multipart(rename_all = \"camelCase\", strict)] struct T { }",
        true
    )]
    #[case::after_list_key("#[try_from_multipart(x(y), strict)] struct T { }", true)]
    // L10 (round 12): the value is read (`strict = false` used to be true).
    #[case::explicit_true("#[try_from_multipart(strict = true)] struct T { }", true)]
    #[case::explicit_false("#[try_from_multipart(strict = false)] struct T { }", false)]
    fn test_extract_strict(#[case] code: &str, #[case] expected: bool) {
        assert_eq!(extract_strict(&parse_struct_attrs(code)).unwrap(), expected);
    }

    #[test]
    fn malformed_try_from_multipart_is_an_error() {
        let attrs = parse_struct_attrs("#[try_from_multipart(strict = )] struct T { }");
        assert!(extract_strict(&attrs).is_err());
        let attrs = parse_struct_attrs("#[try_from_multipart(strict = \"yes\")] struct T { }");
        assert!(extract_strict(&attrs).is_err());
    }

    #[rstest]
    #[case::present("#[form_data(default)] pub x: i32", true)]
    #[case::absent("pub x: i32", false)]
    #[case::other_key(r#"#[form_data(limit = "100")] pub x: i32"#, false)]
    #[case::after_other_key(r#"#[form_data(field_name = "y", default)] pub x: i32"#, true)]
    // L10 (round 12): `default = false` used to read as `default`.
    #[case::explicit_true("#[form_data(default = true)] pub x: i32", true)]
    #[case::explicit_false("#[form_data(default = false)] pub x: i32", false)]
    fn test_form_data_default(#[case] code: &str, #[case] expected: bool) {
        assert_eq!(form_data(code).default, expected);
    }

    #[test]
    fn non_bool_form_data_default_is_an_error() {
        let attrs = parse_attrs("#[form_data(default = 1)] pub x: i32");
        assert!(parse_form_data(&attrs).is_err());
    }

    #[test]
    fn test_extract_struct_default() {
        assert!(matches!(
            extract_struct_default(&parse_struct_attrs("#[serde(default)] struct T { }")),
            DefaultKind::Trait
        ));
        assert!(matches!(
            extract_struct_default(&parse_struct_attrs(
                r#"#[serde(default = "make_t")] struct T { }"#
            )),
            DefaultKind::Function(ref f) if f == "make_t"
        ));
        assert!(matches!(
            extract_struct_default(&parse_struct_attrs("struct T { }")),
            DefaultKind::None
        ));
    }

    #[test]
    fn test_resolve_default_kind() {
        let kind = |code: &str, struct_default: bool| {
            let attrs = parse_attrs(code);
            let form = parse_form_data(&attrs).unwrap();
            resolve_default_kind(&attrs, form.default, struct_default)
        };
        assert!(matches!(kind("pub x: i32", false), DefaultKind::None));
        assert!(matches!(
            kind("#[serde(default)] pub x: i32", false),
            DefaultKind::Trait
        ));
        assert!(
            matches!(kind(r#"#[serde(default = "my_fn")] pub x: i32"#, false), DefaultKind::Function(ref f) if f == "my_fn")
        );
        assert!(matches!(
            kind("#[form_data(default)] pub x: i32", false),
            DefaultKind::Trait
        ));
        assert!(matches!(kind("pub x: i32", true), DefaultKind::Container));
        assert!(matches!(
            kind("#[form_data(default)] pub x: i32", true),
            DefaultKind::Trait
        ));
    }

    #[rstest]
    #[case::none("pub x: String", "std :: option :: Option :: None")]
    #[case::bytes(
        r#"#[form_data(limit = "100")] pub x: String"#,
        "std :: option :: Option :: Some (100usize)"
    )]
    // `"unlimited"` emits the `usize::MAX` unbounded sentinel (not `None`) so
    // the runtime can tell an explicit opt-out apart from an absent attribute
    // and still apply a default cap to the latter.
    #[case::unlimited(
        r#"#[form_data(limit = "unlimited")] pub x: String"#,
        "std :: option :: Option :: Some (usize :: MAX)"
    )]
    #[case::mib(
        r#"#[form_data(limit = "10MiB")] pub x: String"#,
        "std :: option :: Option :: Some (10485760usize)"
    )]
    #[case::after_field_name(
        r#"#[form_data(field_name = "f", limit = "1KiB")] pub x: String"#,
        "std :: option :: Option :: Some (1024usize)"
    )]
    #[case::first_wins(
        r#"#[form_data(limit = "1B")] #[form_data(limit = "2B")] pub x: String"#,
        "std :: option :: Option :: Some (1usize)"
    )]
    fn test_extract_limit_tokens(#[case] code: &str, #[case] expected: &str) {
        let form = form_data(code);
        assert_eq!(
            extract_limit_tokens(form.limit.as_ref()).to_string(),
            expected
        );
    }

    #[rstest]
    #[case::valueless(r"#[form_data(limit)] pub file: String", "expected `limit")]
    #[case::non_string(r"#[form_data(limit = 10)] pub file: String", "expected `limit")]
    #[case::invalid_size(
        r#"#[form_data(limit = "not-a-size")] pub file: String"#,
        "invalid `limit`"
    )]
    #[case::overflow(
        r#"#[form_data(limit = "18446744073709551615KiB")] pub file: String"#,
        "overflows"
    )]
    #[case::malformed(r"#[form_data(limit = )] pub file: String", "expected")]
    #[case::second_invalid_size(
        r#"#[form_data(limit = "10MiB")] #[form_data(limit = "10mb")] pub file: String"#,
        "invalid `limit`"
    )]
    #[case::second_non_string(
        r#"#[form_data(limit = "10MiB")] #[form_data(limit = 10)] pub file: String"#,
        "expected `limit"
    )]
    fn malformed_limit_is_an_error(#[case] code: &str, #[case] message: &str) {
        let Err(err) = parse_form_data(&parse_attrs(code)) else {
            panic!("`{code}` must be rejected");
        };
        assert!(err.to_string().contains(message), "{err}");
    }

    #[test]
    fn extract_limit_tokens_reports_an_unvalidated_literal() {
        let lit: syn::LitStr = syn::parse_quote!("garbage");
        assert!(
            extract_limit_tokens(Some(&lit))
                .to_string()
                .contains("compile_error")
        );
    }

    /// A `#[form_data(default)]` field gets a request view in which it is not
    /// `required` (the derive fills a missing part with `Default`).
    #[test]
    fn form_data_default_field_is_optional_in_the_request_view() {
        use crate::parser::schema::request_view;
        use std::collections::{BTreeSet, HashMap, HashSet};

        let item: syn::Item =
            syn::parse_str("struct Up { #[form_data(default)] pub count: i32, pub name: String }")
                .unwrap();
        let names = request_view::compute_input_names(
            &[request_view::ViewSource {
                name: "Up",
                rust_ident: None,
                scoped_keys: Vec::new(),
                item: &item,
            }],
            &BTreeSet::new(),
        );
        assert!(names.contains_key("Up"));
        let syn::Item::Struct(item) = &item else {
            unreachable!()
        };
        let schema = request_view::with_request_view(|| {
            crate::parser::parse_struct_to_schema(
                item,
                &HashSet::<String>::new(),
                &HashMap::<String, String>::new(),
            )
        });
        assert_eq!(schema.required, Some(vec!["name".to_string()]));
        assert!(!has_form_data_default(&parse_attrs(
            "#[form_data(=)] pub x: i32"
        )));
    }

    /// L4 (round 18): `#[try_from_multipart(strict)]` rejects unknown parts,
    /// so the schema closes the object as under `deny_unknown_fields` (it
    /// stayed open); `strict = false` and a broken attribute keep it open.
    #[rstest::rstest]
    #[case::strict("#[try_from_multipart(strict)]", true)]
    #[case::strict_false("#[try_from_multipart(strict = false)]", false)]
    #[case::broken("#[try_from_multipart(strict = 1)]", false)]
    #[case::lenient("#[try_from_multipart(rename_all = \"camelCase\")]", false)]
    fn strict_multipart_closes_the_schema(#[case] attr: &str, #[case] closed: bool) {
        use std::collections::{HashMap, HashSet};
        let item: syn::ItemStruct =
            syn::parse_str(&format!("{attr} struct Up {{ pub name: String }}")).unwrap();
        assert_eq!(
            crate::multipart_impl::rejects_unknown_parts(&item.attrs),
            closed
        );
        let schema = crate::parser::parse_struct_to_schema(
            &item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        assert_eq!(
            schema.additional_properties.is_some(),
            closed,
            "{:?}",
            schema.additional_properties
        );
    }
}
