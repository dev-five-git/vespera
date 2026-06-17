use proc_macro2::TokenStream;
use quote::quote;

use super::fields::DefaultKind;
use super::types::{parse_byte_unit, strip_raw_prefix};
use crate::parser::{extract_default, extract_field_rename, rename_field};

/// Resolve the multipart field name using serde + form_data attributes.
///
/// Priority:
/// 1. `#[form_data(field_name = "...")]`
/// 2. `#[serde(rename = "...")]`
/// 3. struct-level `rename_all` applied to Rust field name
/// 4. Rust field name as-is
pub(super) fn resolve_field_name(
    ident: &syn::Ident,
    attrs: &[syn::Attribute],
    rename_all: Option<&str>,
) -> String {
    if let Some(name) = extract_form_data_field_name(attrs) {
        return name;
    }
    if let Some(name) = extract_field_rename(attrs) {
        return name;
    }
    let rust_name = strip_raw_prefix(&ident.to_string());
    rename_field(&rust_name, rename_all)
}

/// Extract `field_name` from `#[form_data(field_name = "...")]`.
fn extract_form_data_field_name(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut found = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("field_name")
                    && let Ok(value) = meta.value()
                    && let Ok(lit) = value.parse::<syn::LitStr>()
                {
                    found = Some(lit.value());
                }
                Ok(())
            });
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

/// Extract `strict` flag from `#[try_from_multipart(strict)]`.
pub(super) fn extract_strict(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("try_from_multipart") {
            let mut strict = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("strict") {
                    strict = true;
                }
                Ok(())
            });
            if strict {
                return true;
            }
        }
    }
    false
}

/// Extract `limit` from `#[form_data(limit = "10MiB")]` and emit as `Option<usize>` tokens.
pub(super) fn extract_limit_tokens(attrs: &[syn::Attribute]) -> TokenStream {
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut limit_str = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("limit")
                    && let Ok(value) = meta.value()
                    && let Ok(lit) = value.parse::<syn::LitStr>()
                {
                    limit_str = Some(lit.value());
                }
                Ok(())
            });
            if let Some(s) = limit_str {
                if s == "unlimited" {
                    // `usize::MAX` is the explicit unbounded sentinel: every
                    // limit check (`total > limit`) is byte-for-byte
                    // equivalent to the former `None` (never triggers), but
                    // it is DISTINGUISHABLE from an ABSENT attribute (which
                    // stays `None` below).  That lets the runtime apply a
                    // default cap to unannotated text fields (`String`) while
                    // an explicit `limit = "unlimited"` opt-out stays
                    // genuinely unbounded.
                    return quote! { std::option::Option::Some(usize::MAX) };
                }
                if let Some(bytes) = parse_byte_unit(&s) {
                    return quote! { std::option::Option::Some(#bytes) };
                }
            }
        }
    }
    quote! { std::option::Option::None }
}

/// Whether the field carries an explicit, VALID `#[form_data(limit = ...)]`
/// — either `"unlimited"` or a parseable byte size (e.g. `"10MiB"`).
///
/// An absent attribute, a non-`limit` `form_data` key, or an unparseable
/// value all return `false`. The `Multipart` derive treats that as a
/// missing limit on a file field and emits a compile error, so an unbounded
/// upload is never accepted silently.
pub(super) fn has_explicit_limit(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut valid = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("limit")
                    && let Ok(value) = meta.value()
                    && let Ok(lit) = value.parse::<syn::LitStr>()
                {
                    let s = lit.value();
                    valid = s == "unlimited" || parse_byte_unit(&s).is_some();
                }
                Ok(())
            });
            if valid {
                return true;
            }
        }
    }
    false
}

/// Resolve the default behavior for a field.
///
/// Priority:
/// 1. `#[form_data(default)]` — explicit form_data override (bare default)
/// 2. `#[serde(default)]` — bare default via `Default::default()`
/// 3. `#[serde(default = "fn_path")]` — custom default function
/// 4. Struct-level `#[serde(default)]` — all fields get `Default::default()`
/// 5. No default — field is required
pub(super) fn resolve_default_kind(attrs: &[syn::Attribute], struct_default: bool) -> DefaultKind {
    if extract_form_data_default(attrs) {
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

/// Extract `default` flag from `#[form_data(default)]`.
fn extract_form_data_default(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut has_default = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    has_default = true;
                }
                Ok(())
            });
            if has_default {
                return true;
            }
        }
    }
    false
}

/// Check if the struct has `#[serde(default)]` at the struct level.
pub(super) fn extract_struct_default(attrs: &[syn::Attribute]) -> bool {
    extract_default(attrs).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_extract_form_data_field_name_present() {
        let attrs = parse_attrs(r#"#[form_data(field_name = "custom")] pub x: String"#);
        assert_eq!(
            extract_form_data_field_name(&attrs),
            Some("custom".to_string())
        );
    }

    #[test]
    fn test_extract_form_data_field_name_absent() {
        assert_eq!(
            extract_form_data_field_name(&parse_attrs("pub x: String")),
            None
        );
    }

    #[test]
    fn test_extract_form_data_field_name_other_form_data_attr() {
        let attrs = parse_attrs(r#"#[form_data(limit = "100")] pub x: String"#);
        assert_eq!(extract_form_data_field_name(&attrs), None);
    }

    #[test]
    fn test_extract_strict_present() {
        let attrs = parse_struct_attrs("#[try_from_multipart(strict)] struct T { }");
        assert!(extract_strict(&attrs));
    }

    #[test]
    fn test_extract_strict_absent() {
        let attrs = parse_struct_attrs("struct T { }");
        assert!(!extract_strict(&attrs));
    }

    #[test]
    fn test_extract_strict_other_attr() {
        let attrs =
            parse_struct_attrs("#[try_from_multipart(rename_all = \"camelCase\")] struct T { }");
        assert!(!extract_strict(&attrs));
    }

    #[test]
    fn test_extract_form_data_default_present() {
        assert!(extract_form_data_default(&parse_attrs(
            "#[form_data(default)] pub x: i32"
        )));
    }

    #[test]
    fn test_extract_form_data_default_absent() {
        assert!(!extract_form_data_default(&parse_attrs("pub x: i32")));
    }

    #[test]
    fn test_extract_form_data_default_other_form_data() {
        let attrs = parse_attrs(r#"#[form_data(limit = "100")] pub x: i32"#);
        assert!(!extract_form_data_default(&attrs));
    }

    #[test]
    fn test_extract_struct_default_present() {
        let attrs = parse_struct_attrs("#[serde(default)] struct T { }");
        assert!(extract_struct_default(&attrs));
    }

    #[test]
    fn test_extract_struct_default_absent() {
        let attrs = parse_struct_attrs("struct T { }");
        assert!(!extract_struct_default(&attrs));
    }

    #[test]
    fn test_resolve_default_kind_none() {
        let attrs = parse_attrs("pub x: i32");
        assert!(matches!(
            resolve_default_kind(&attrs, false),
            DefaultKind::None
        ));
    }

    #[test]
    fn test_resolve_default_kind_serde_default() {
        let attrs = parse_attrs("#[serde(default)] pub x: i32");
        assert!(matches!(
            resolve_default_kind(&attrs, false),
            DefaultKind::Trait
        ));
    }

    #[test]
    fn test_resolve_default_kind_serde_default_fn() {
        let attrs = parse_attrs(r#"#[serde(default = "my_fn")] pub x: i32"#);
        assert!(
            matches!(resolve_default_kind(&attrs, false), DefaultKind::Function(ref f) if f == "my_fn")
        );
    }

    #[test]
    fn test_resolve_default_kind_form_data_default() {
        let attrs = parse_attrs("#[form_data(default)] pub x: i32");
        assert!(matches!(
            resolve_default_kind(&attrs, false),
            DefaultKind::Trait
        ));
    }

    #[test]
    fn test_resolve_default_kind_struct_level() {
        let attrs = parse_attrs("pub x: i32");
        assert!(matches!(
            resolve_default_kind(&attrs, true),
            DefaultKind::Trait
        ));
    }

    #[test]
    fn test_resolve_default_kind_form_data_overrides_struct_default() {
        let attrs = parse_attrs("#[form_data(default)] pub x: i32");
        assert!(matches!(
            resolve_default_kind(&attrs, true),
            DefaultKind::Trait
        ));
    }

    #[test]
    fn test_resolve_field_name_plain() {
        let field = parse_field("pub my_field: String");
        let name = resolve_field_name(field.ident.as_ref().unwrap(), &field.attrs, None);
        assert_eq!(name, "my_field");
    }

    #[test]
    fn test_resolve_field_name_rename_all() {
        let field = parse_field("pub my_field: String");
        let name = resolve_field_name(
            field.ident.as_ref().unwrap(),
            &field.attrs,
            Some("camelCase"),
        );
        assert_eq!(name, "myField");
    }

    #[test]
    fn test_resolve_field_name_serde_rename() {
        let field = parse_field(r#"#[serde(rename = "custom")] pub my_field: String"#);
        let name = resolve_field_name(
            field.ident.as_ref().unwrap(),
            &field.attrs,
            Some("camelCase"),
        );
        assert_eq!(name, "custom");
    }

    #[test]
    fn test_resolve_field_name_form_data_field_name() {
        let field = parse_field(
            r#"#[form_data(field_name = "override")] #[serde(rename = "serde_name")] pub my_field: String"#,
        );
        let name = resolve_field_name(
            field.ident.as_ref().unwrap(),
            &field.attrs,
            Some("camelCase"),
        );
        assert_eq!(name, "override");
    }

    #[test]
    fn test_extract_limit_tokens_none() {
        assert_eq!(
            extract_limit_tokens(&parse_attrs("pub x: String")).to_string(),
            "std :: option :: Option :: None"
        );
    }

    #[test]
    fn test_extract_limit_tokens_with_value() {
        let attrs = parse_attrs(r#"#[form_data(limit = "100")] pub x: String"#);
        assert_eq!(
            extract_limit_tokens(&attrs).to_string(),
            "std :: option :: Option :: Some (100usize)"
        );
    }

    #[test]
    fn test_extract_limit_tokens_unlimited() {
        // `"unlimited"` now emits the `usize::MAX` unbounded sentinel (not
        // `None`) so the runtime can tell an explicit opt-out apart from an
        // absent attribute and still apply a default cap to the latter.
        let attrs = parse_attrs(r#"#[form_data(limit = "unlimited")] pub x: String"#);
        assert_eq!(
            extract_limit_tokens(&attrs).to_string(),
            "std :: option :: Option :: Some (usize :: MAX)"
        );
    }

    #[test]
    fn test_extract_limit_tokens_mib() {
        let attrs = parse_attrs(r#"#[form_data(limit = "10MiB")] pub x: String"#);
        let expected = 10 * 1024 * 1024;
        assert_eq!(
            extract_limit_tokens(&attrs).to_string(),
            format!("std :: option :: Option :: Some ({expected}usize)")
        );
    }

    #[test]
    fn test_has_explicit_limit_size() {
        assert!(has_explicit_limit(&parse_attrs(
            r#"#[form_data(limit = "10MiB")] pub x: String"#
        )));
        assert!(has_explicit_limit(&parse_attrs(
            r#"#[form_data(limit = "100")] pub x: String"#
        )));
    }

    #[test]
    fn test_has_explicit_limit_unlimited() {
        assert!(has_explicit_limit(&parse_attrs(
            r#"#[form_data(limit = "unlimited")] pub x: String"#
        )));
    }

    #[test]
    fn test_has_explicit_limit_absent() {
        assert!(!has_explicit_limit(&parse_attrs("pub x: String")));
    }

    #[test]
    fn test_has_explicit_limit_invalid_value() {
        // An unparseable size is NOT a valid limit — treated as missing so a
        // file field with `limit = "garbage"` still fails the derive check.
        assert!(!has_explicit_limit(&parse_attrs(
            r#"#[form_data(limit = "garbage")] pub x: String"#
        )));
    }

    #[test]
    fn test_has_explicit_limit_other_form_data_key() {
        // A `form_data` attr without a `limit` key does not count.
        assert!(!has_explicit_limit(&parse_attrs(
            r#"#[form_data(field_name = "x")] pub x: String"#
        )));
    }
}
