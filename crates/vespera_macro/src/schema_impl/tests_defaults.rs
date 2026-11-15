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

/// A file with a top-level `role` default and inline modules holding their
/// own, so a lookup from the wrong module returns the wrong value.
const NESTED_DEFAULTS: &str = r#"
fn role() -> String { "top".to_string() }
mod admin {
    fn role() -> String { "admin".to_string() }
    pub struct Account { #[serde(default = "role")] role: String }
    mod nested {
        pub struct Account { #[serde(default = "super::role")] role: String }
    }
    mod presets {
        pub fn role() -> String { "preset".to_string() }
    }
    pub struct Preset { #[serde(default = "presets::role")] role: String }
    pub struct SelfRole { #[serde(default = "self::role")] role: String }
}
mod guest {
    fn role() -> String { "guest".to_string() }
    pub struct Account { #[serde(default = "role")] role: String }
}
pub struct Top { #[serde(default = "role")] role: String }
pub struct Escapes { #[serde(default = "super::role")] role: String }
pub struct Absolute { #[serde(default = "::defaults::role")] role: String }
pub struct Garbage { #[serde(default = "not a path")] role: String }
"#;

/// The struct `name` in `NESTED_DEFAULTS` reached through the inline module
/// `chain`, parsed from the file text so its ident keeps its real line.
fn nested_struct(chain: &[&str], name: &str) -> syn::DeriveInput {
    let mut items = syn::parse_file(NESTED_DEFAULTS).unwrap().items;
    for segment in chain {
        items = items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Mod(module) if module.ident == segment => {
                    module.content.map(|(_, inner)| inner)
                }
                _ => None,
            })
            .unwrap();
    }
    items
        .into_iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == name => Some(item.into()),
            _ => None,
        })
        .unwrap()
}

/// A default function is looked up from the module declaring the struct:
/// two same-named structs in different inline modules each find their own,
/// `self::` / `super::` / `child::f` walk the inline modules, and paths that
/// leave the file (`super` above the top level, a leading `::`) or do not
/// parse resolve to nothing.
#[rstest]
#[case::inline_module(&["admin"], "Account", Some("admin"))]
#[case::other_inline_module(&["guest"], "Account", Some("guest"))]
#[case::super_path(&["admin", "nested"], "Account", Some("admin"))]
#[case::child_module_path(&["admin"], "Preset", Some("preset"))]
#[case::self_path(&["admin"], "SelfRole", Some("admin"))]
#[case::top_level(&[], "Top", Some("top"))]
#[case::super_above_file(&[], "Escapes", None)]
#[case::leading_colon(&[], "Absolute", None)]
#[case::unparseable(&[], "Garbage", None)]
fn default_fn_is_resolved_from_the_struct_module(
    #[case] chain: &[&str],
    #[case] name: &str,
    #[case] expected: Option<&str>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("nested.rs");
    std::fs::write(&file_path, NESTED_DEFAULTS).unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let input = nested_struct(chain, name);
    let defaults = extract_field_defaults_from_path(&input, &file_path);

    assert_eq!(
        defaults.get("role"),
        expected
            .map(|value| serde_json::Value::String(value.to_string()))
            .as_ref()
    );
}

/// A `crate::..` path — what `schema_type!` re-roots a source field's
/// `default = "fn"` to — is looked up in that module, so the generated DTO's
/// `#[derive(Schema)]` resolves the default instead of rejecting the field.
#[rstest]
#[case::found("crate::models::user::default_role", Some("member"))]
#[case::missing_function("crate::models::user::missing", None)]
#[case::missing_module("crate::models::nope::default_role", None)]
fn crate_path_default_fn_is_resolved_through_the_module_tree(
    #[case] fn_path: &str,
    #[case] expected: Option<&str>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let models = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(temp_dir.path().join("src").join("lib.rs"), "mod models;").unwrap();
    std::fs::write(models.join("mod.rs"), "pub mod user;").unwrap();
    std::fs::write(
        models.join("user.rs"),
        "pub fn default_role() -> String { \"member\".to_string() }",
    )
    .unwrap();
    let call_site = temp_dir.path().join("src").join("dto.rs");
    std::fs::write(&call_site, "").unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    crate::schema_macro::file_cache::bump_epoch();

    let input: syn::DeriveInput = syn::parse_str(&format!(
        "struct UserDto {{ #[serde(default = \"{fn_path}\")] role: Opaque }}"
    ))
    .unwrap();
    let defaults = extract_field_defaults_from_path(&input, &call_site);

    assert_eq!(
        defaults.get("role"),
        expected
            .map(|value| serde_json::Value::String(value.to_string()))
            .as_ref()
    );
    assert_eq!(
        validate_serde_default_values(&input, &defaults).is_ok(),
        expected.is_some()
    );
}
