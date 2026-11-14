//! `#[schema(default)]` / `#[serde(default)]` detection behind the
//! derive-time default validation.

use std::collections::BTreeMap;

use rstest::rstest;

use super::*;

/// A `#[schema(default)]` key is found wherever it sits in the list: an
/// earlier key's value (a type for `value_type`) never ends the search.
#[rstest]
#[case::after_format(quote::quote!(#[schema(format = "uuid", default = "x")]), true)]
#[case::after_value_type(quote::quote!(#[schema(value_type = Vec<String>, default = "x")]), true)]
#[case::non_string_value(quote::quote!(#[schema(default = 5)]), true)]
#[case::bare(quote::quote!(#[schema(default)]), true)]
#[case::other_keys_only(quote::quote!(#[schema(format = "uuid", nullable)]), false)]
fn schema_default_found_after_other_keys(
    #[case] attrs: proc_macro2::TokenStream,
    #[case] expected: bool,
) {
    let item: syn::ItemStruct = syn::parse2(quote::quote!(struct S { #attrs f: i32 })).unwrap();
    assert_eq!(
        has_schema_default(&item.fields.iter().next().unwrap().attrs),
        expected
    );
}

/// `#[serde(default)]` on an unresolvable type is accepted when an explicit
/// `#[schema(default = "..")]` follows another schema key.
#[test]
fn explicit_schema_default_after_other_key_satisfies_validation() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct S {
            #[serde(default)]
            #[schema(format = "uuid", default = "00000000-0000-0000-0000-000000000000")]
            id: Opaque,
        }
    };
    assert!(validate_serde_default_values(&input, &BTreeMap::new()).is_ok());
}

/// A raw-identifier field (`r#type`) is keyed by its wire name `type`, which
/// is what the OpenAPI default lookup and the validation use.
#[test]
fn raw_identifier_field_default_is_keyed_without_prefix() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("defaults.rs");
    std::fs::write(
        &file_path,
        "fn default_kind() -> String { \"plain\".to_string() }",
    )
    .unwrap();
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Config {
            #[serde(default = "default_kind")]
            r#type: Opaque,
        }
    };

    let defaults = extract_field_defaults_from_path(&input, &file_path);
    assert_eq!(
        defaults.get("type"),
        Some(&serde_json::Value::String("plain".to_string()))
    );
    assert!(validate_serde_default_values(&input, &defaults).is_ok());
}
