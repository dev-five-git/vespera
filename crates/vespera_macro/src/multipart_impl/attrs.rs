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
    /// `field_name = "..."` (a non-string value is ignored).
    pub(super) field_name: Option<String>,
    /// `limit = "..."`, validated: `"unlimited"` or a byte size.
    pub(super) limit: Option<syn::LitStr>,
    /// Bare `default`.
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
/// wins; malformed syntax and an invalid `limit` are spanned errors.
pub(super) fn parse_form_data(attrs: &[syn::Attribute]) -> syn::Result<FormDataAttrs> {
    let mut out = FormDataAttrs::default();
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("form_data"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("field_name") {
                let expr = value_expr(&meta)?;
                if out.field_name.is_none() {
                    out.field_name = as_lit_str(expr.as_ref()).map(syn::LitStr::value);
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
                if out.limit.is_none() {
                    // Validate now so the error points at the literal.
                    limit_tokens(lit)?;
                    out.limit = Some(lit.clone());
                }
            } else {
                out.default |= meta.path.is_ident("default");
                skip_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
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
            strict |= meta.path.is_ident("strict");
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
/// 4. Struct-level `#[serde(default)]` — all fields get `Default::default()`
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
        return DefaultKind::Trait;
    }
    DefaultKind::None
}

/// Check if the struct has `#[serde(default)]` at the struct level.
pub(super) fn extract_struct_default(attrs: &[syn::Attribute]) -> bool {
    extract_default(attrs).is_some()
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
    #[case::non_string(r"#[form_data(field_name = 42)] pub x: String", None)]
    #[case::valueless(r"#[form_data(field_name)] pub x: String", None)]
    #[case::unknown_list_key(
        r#"#[form_data(other(a = 1), field_name = "y")] pub x: String"#,
        Some("y")
    )]
    fn field_name_is_read_in_any_position(#[case] code: &str, #[case] expected: Option<&str>) {
        assert_eq!(form_data(code).field_name.as_deref(), expected);
    }

    #[rstest]
    #[case::malformed_field_name(
        r#"#[form_data(field_name = 42)] #[serde(rename = "wire_name")] pub rust_name: String"#,
        None,
        "wire_name"
    )]
    #[case::valueless_field_name(
        r"#[form_data(field_name)] pub rust_name: String",
        None,
        "rust_name"
    )]
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
    fn test_extract_strict(#[case] code: &str, #[case] expected: bool) {
        assert_eq!(extract_strict(&parse_struct_attrs(code)).unwrap(), expected);
    }

    #[test]
    fn malformed_try_from_multipart_is_an_error() {
        let attrs = parse_struct_attrs("#[try_from_multipart(strict = )] struct T { }");
        assert!(extract_strict(&attrs).is_err());
    }

    #[rstest]
    #[case::present("#[form_data(default)] pub x: i32", true)]
    #[case::absent("pub x: i32", false)]
    #[case::other_key(r#"#[form_data(limit = "100")] pub x: i32"#, false)]
    #[case::after_other_key(r#"#[form_data(field_name = "y", default)] pub x: i32"#, true)]
    fn test_form_data_default(#[case] code: &str, #[case] expected: bool) {
        assert_eq!(form_data(code).default, expected);
    }

    #[test]
    fn test_extract_struct_default() {
        assert!(extract_struct_default(&parse_struct_attrs(
            "#[serde(default)] struct T { }"
        )));
        assert!(!extract_struct_default(&parse_struct_attrs("struct T { }")));
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
        assert!(matches!(kind("pub x: i32", true), DefaultKind::Trait));
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
}
