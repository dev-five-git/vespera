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
    assert!(validate_serde_default_values(&input, &BTreeMap::new(), true).is_ok());
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
    assert!(validate_serde_default_values(&input, &defaults, true).is_ok());
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
/// `self::` / `super::` / `child::f` walk the inline modules, and a leading
/// `::` or an unparseable path resolves to nothing. `super` above the top
/// level goes through the module tree (`super_past_file` below); this file
/// lies outside the crate's module directory, so it maps to the crate root
/// and there is no parent module to resolve it in.
#[rstest]
#[case::inline_module(&["admin"], "Account", Some("admin"))]
#[case::other_inline_module(&["guest"], "Account", Some("guest"))]
#[case::super_path(&["admin", "nested"], "Account", Some("admin"))]
#[case::child_module_path(&["admin"], "Preset", Some("preset"))]
#[case::self_path(&["admin"], "SelfRole", Some("admin"))]
#[case::top_level(&[], "Top", Some("top"))]
#[case::super_above_file_outside_crate(&[], "Escapes", None)]
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

/// A file whose fn `build` declares a struct in its body inside `mod admin`:
/// the struct's bare `role` default is `admin`'s, not the top level's.
const FN_BODY_DEFAULTS: &str = r#"
fn role() -> String { "top".to_string() }
mod admin {
    fn role() -> String { "admin".to_string() }
    fn build() {
        struct Built { #[serde(default = "role")] role: String }
    }
}
"#;

/// SM0-L3 (round 38): a `#[derive(Schema)]` struct inside a fn inside a
/// module resolves its default from that module. A struct the file does not
/// declare (a `schema_type!` DTO) keeps the file's top-level lookup.
#[rstest]
#[case::declared_in_a_fn_body("Built", Some("admin"))]
#[case::not_declared_in_the_file("Ghost", Some("top"))]
fn default_fn_of_a_struct_in_a_fn_body_is_resolved_from_its_module(
    #[case] name: &str,
    #[case] expected: Option<&str>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("fn_body.rs");
    std::fs::write(&file_path, FN_BODY_DEFAULTS).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let input: syn::DeriveInput = syn::parse_str(&format!(
        "struct {name} {{ #[serde(default = \"role\")] role: String }}"
    ))
    .unwrap();
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
#[case::reexported("crate::models::reexported_role", Some("inner"))]
#[case::reexport_missing("crate::models::absent_role", None)]
fn crate_path_default_fn_is_resolved_through_the_module_tree(
    #[case] fn_path: &str,
    #[case] expected: Option<&str>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let models = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(temp_dir.path().join("src").join("lib.rs"), "mod models;").unwrap();
    std::fs::write(
        models.join("mod.rs"),
        "pub mod user;
mod inner;
pub use inner::role as reexported_role;",
    )
    .unwrap();
    std::fs::write(
        models.join("user.rs"),
        "pub fn default_role() -> String { \"member\".to_string() }",
    )
    .unwrap();
    std::fs::write(
        models.join("inner.rs"),
        "pub fn role() -> String { \"inner\".to_string() }",
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
        validate_serde_default_values(&input, &defaults, true).is_ok(),
        expected.is_some()
    );
}

/// Paths the in-file walk cannot follow resolve through the module tree: an
/// out-of-line child module, `super::` above the file's top level, and a
/// `use`-imported name (they used to demand an explicit `#[schema(default)]`);
/// a function another crate exports cannot be read.
#[rstest]
#[case::out_of_line_child("A", Some("child"))]
#[case::super_past_file("B", Some("parent"))]
#[case::imported_name("C", Some("imported"))]
#[case::missing("D", None)]
#[case::external_crate("E", None)]
fn default_fn_is_resolved_through_the_module_tree(
    #[case] name: &str,
    #[case] expected: Option<&str>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let src = temp_dir.path().join("src");
    let files = [
        ("lib.rs", "pub mod models;\npub mod shared;"),
        (
            "shared.rs",
            "pub fn imported_role() -> String { \"imported\".to_string() }",
        ),
        (
            "models/mod.rs",
            "pub mod user;\npub fn parent_role() -> String { \"parent\".to_string() }",
        ),
        (
            "models/user/defaults.rs",
            "pub fn role() -> String { \"child\".to_string() }",
        ),
    ];
    for (path, content) in files {
        std::fs::create_dir_all(src.join(path).parent().unwrap()).unwrap();
        std::fs::write(src.join(path), content).unwrap();
    }
    let user = "mod defaults;\nuse crate::shared::imported_role;\nuse other_crate::role as foreign_role;\n\
        pub struct A { #[serde(default = \"defaults::role\")] role: String }\n\
        pub struct B { #[serde(default = \"super::parent_role\")] role: String }\n\
        pub struct C { #[serde(default = \"imported_role\")] role: String }\n\
        pub struct D { #[serde(default = \"nowhere::role\")] role: String }\n\
        pub struct E { #[serde(default = \"foreign_role\")] role: String }\n";
    let file_path = src.join("models/user.rs");
    std::fs::write(&file_path, user).unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    crate::schema_macro::file_cache::bump_epoch();

    let input = syn::parse_file(user)
        .unwrap()
        .items
        .into_iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == name => Some(syn::DeriveInput::from(item)),
            _ => None,
        })
        .unwrap();
    let defaults = extract_field_defaults_from_path(&input, &file_path);
    assert_eq!(
        defaults.get("role"),
        expected
            .map(|value| serde_json::Value::String(value.to_string()))
            .as_ref()
    );
}

/// L12 (round 12): without the call-site file (`Span::local_file` is `None`)
/// no default function can be looked up, so a `default = "fn"` field is not
/// rejected as unresolvable; a bare default is still checked.
#[rstest]
#[case::function(quote::quote!(#[serde(default = "make")]), false, true)]
#[case::function_file_known(quote::quote!(#[serde(default = "make")]), true, false)]
#[case::bare_opaque(quote::quote!(#[serde(default)]), false, false)]
fn function_default_without_call_site_file_is_not_checked(
    #[case] attrs: proc_macro2::TokenStream,
    #[case] file_known: bool,
    #[case] ok: bool,
) {
    let input: syn::DeriveInput =
        syn::parse2(quote::quote!(struct S { #attrs client: Client })).unwrap();
    assert_eq!(
        validate_serde_default_values(&input, &BTreeMap::new(), file_known).is_ok(),
        ok
    );
    if !file_known {
        // `process_derive_schema` runs without a call-site file in unit tests.
        assert_eq!(process_derive_schema(&input).0.is_some(), ok);
    }
}

/// L11 (round 12): the Multipart derive fills a `#[form_data(default)]`
/// field with `Default::default()`, so that (not the serde function's
/// value) is the documented default; a type without a static default
/// needs an explicit `#[schema(default)]`.
#[test]
fn form_data_default_decides_the_documented_default() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("upload.rs");
    std::fs::write(&file_path, "fn seven() -> i32 { 7 }").unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Upload {
            #[serde(default = "seven")]
            #[form_data(default)]
            n: i32,
            #[serde(default = "seven")]
            m: i32,
            #[serde(default = "seven")]
            #[form_data(default)]
            o: Opaque,
        }
    };
    let defaults = extract_field_defaults_from_path(&input, &file_path);
    assert_eq!(defaults.get("n"), Some(&serde_json::json!(0)));
    assert_eq!(defaults.get("m"), Some(&serde_json::json!(7)));
    assert_eq!(defaults.get("o"), None);
    let err = validate_serde_default_values(&input, &defaults, true).unwrap_err();
    assert!(err.to_string().contains("field `o`"), "{err}");
}

/// A field serde never reads (`skip` / `skip_deserializing`) has no request
/// property, so an unresolvable `default = "fn"` on it is not an error.
#[rstest]
#[case::skip(quote::quote!(#[serde(skip, default = "new_client")]), true)]
#[case::skip_deserializing(quote::quote!(#[serde(skip_deserializing, default = "new_client")]), true)]
#[case::read(quote::quote!(#[serde(default = "new_client")]), false)]
fn skipped_field_default_is_not_validated(
    #[case] attrs: proc_macro2::TokenStream,
    #[case] ok: bool,
) {
    let input: syn::DeriveInput =
        syn::parse2(quote::quote!(struct S { #attrs client: Client })).unwrap();
    assert_eq!(
        validate_serde_default_values(&input, &BTreeMap::new(), true).is_ok(),
        ok
    );
}

/// L2 (round 13): the per-file default-fn cache is keyed by the unrawed
/// name, as every lookup asks (`default = "r#type"` missed it).
#[test]
fn default_function_cache_keys_are_unrawed() {
    let file = syn::parse_file("fn r#type() -> i32 { 5 } fn plain() -> i32 { 6 }").unwrap();
    let values = extract_default_functions_from_file(&file);
    assert_eq!(values.get("type"), Some(&serde_json::json!(5)));
    assert_eq!(values.get("plain"), Some(&serde_json::json!(6)));
    assert!(!values.contains_key("r#type"));
}

/// M1 (round 14): a std default function (`Vec::new`, `String::new`,
/// `Default::default`, `<T>::default`, ..) is the field type's default
/// instead of a "cannot statically determine" compile error.
#[rstest]
#[case::vec_new("Vec::new", quote::quote!(Vec<String>), Some(serde_json::json!([])))]
#[case::vec_turbofish("Vec::<String>::new", quote::quote!(Vec<String>), Some(serde_json::json!([])))]
#[case::string_new("String::new", quote::quote!(String), Some(serde_json::json!("")))]
#[case::std_path_map("std::collections::HashMap::new", quote::quote!(HashMap<String, i32>), Some(serde_json::json!({})))]
#[case::default_trait("Default::default", quote::quote!(bool), Some(serde_json::json!(false)))]
#[case::std_default_trait("std::default::Default::default", quote::quote!(i32), Some(serde_json::json!(0)))]
#[case::core_default_trait("core::default::Default::default", quote::quote!(i32), Some(serde_json::json!(0)))]
#[case::type_default("i64::default", quote::quote!(i64), Some(serde_json::json!(0)))]
#[case::qself_default("<Vec<i32>>::default", quote::quote!(Vec<i32>), Some(serde_json::json!([])))]
#[case::qself_as_default("<Vec<i32> as Default>::default", quote::quote!(Vec<i32>), Some(serde_json::json!([])))]
#[case::qself_new("<Vec<i32>>::new", quote::quote!(Vec<i32>), Some(serde_json::json!([])))]
#[case::qself_string_new("<String>::new", quote::quote!(String), Some(serde_json::json!("")))]
#[case::qself_new_other_type("<Vec<i32>>::new", quote::quote!(String), None)]
#[case::qself_new_non_container("<Uuid>::new", quote::quote!(Uuid), None)]
#[case::qself_as_new("<Vec<i32> as Other>::new", quote::quote!(Vec<i32>), None)]
#[case::qself_other_fn("<Vec<i32>>::with_capacity", quote::quote!(Vec<i32>), None)]
#[case::qself_other_trait("<Vec<i32> as Other>::default", quote::quote!(Vec<i32>), None)]
// R23-SM2: a qualified `default` names the field's type, or it is another
// type's default (serde calls `Other::default()`, not `[]`).
#[case::qself_default_other_type("<Other>::default", quote::quote!(Vec<String>), None)]
#[case::qself_as_default_other_type("<Other as Default>::default", quote::quote!(Vec<String>), None)]
#[case::other_type_new("Vec::new", quote::quote!(String), None)]
#[case::non_container_new("Uuid::new", quote::quote!(Uuid), None)]
#[case::other_fn("Vec::with_capacity", quote::quote!(Vec<i32>), None)]
#[case::not_a_path_type("Vec::new", quote::quote!([u8; 4]), None)]
#[case::opaque_default("Default::default", quote::quote!(Opaque), None)]
#[case::user_fn("make_tags", quote::quote!(Vec<String>), None)]
#[case::unparsable("not a path", quote::quote!(Vec<String>), None)]
fn std_default_fn_is_the_type_default(
    #[case] fn_path: &str,
    #[case] ty: proc_macro2::TokenStream,
    #[case] expected: Option<serde_json::Value>,
) {
    let ty: syn::Type = syn::parse2(ty).unwrap();
    assert_eq!(default_fns::std_type_default(fn_path, &ty), expected);
}

/// M1 (round 14): `#[derive(Schema)] struct S { #[serde(default = "Vec::new")] .. }`
/// compiles (its default is `[]`), and `Self::f` / `Type::f` resolve an
/// inherent `impl` function of the struct's module. Only `missing`, which
/// names no function, is reported by the validation.
#[test]
fn std_and_associated_default_fns_resolve_and_flag_only_unresolved() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("tags.rs");
    let source = "pub struct S {
        #[serde(default = \"Vec::new\")] tags: Vec<String>,
        #[serde(default = \"Self::role\")] role: Opaque,
        #[serde(default = \"S::level\")] level: Opaque,
        #[serde(default = \"Other::level\")] other: Opaque,
        #[serde(default = \"Self::missing\")] missing: Opaque,
        }
        impl S { fn role() -> String { \"self\".to_string() } fn level() -> i32 { 3 } }
        impl Default for S { fn default() -> Self { todo!() } }
        impl Trait for Other { fn level() -> i32 { 9 } }
        impl Other { fn level() -> i32 { 4 } }
        impl<T> Other<T> { fn unused() -> i32 { 5 } }
        impl <T as Tr>::X { fn level() -> i32 { 6 } }
";
    std::fs::write(&file_path, source).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let input = syn::parse_file(source)
        .unwrap()
        .items
        .into_iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) => Some(syn::DeriveInput::from(item)),
            _ => None,
        })
        .unwrap();
    let defaults = extract_field_defaults_from_path(&input, &file_path);
    assert_eq!(defaults.get("tags"), Some(&serde_json::json!([])));
    assert_eq!(defaults.get("role"), Some(&serde_json::json!("self")));
    assert_eq!(defaults.get("level"), Some(&serde_json::json!(3)));
    assert_eq!(defaults.get("other"), Some(&serde_json::json!(4)));
    assert_eq!(defaults.get("missing"), None);
    let err = validate_serde_default_values(&input, &defaults, true).unwrap_err();
    assert!(err.to_string().contains("field `missing`"), "{err}");
    assert!(!err.to_string().contains("field `tags`"), "{err}");
}

/// M1 (round 15): `schema_type!` spells a source field's `Self::f` as
/// `crate::m::Type::f`; the DTO's derive finds the associated function in
/// `m`'s inherent `impl Type` (it was a "cannot statically determine" error).
#[test]
fn crate_path_associated_default_fn_resolves() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod m; pub mod api;").unwrap();
    std::fs::write(
        src.join("m.rs"),
        "pub struct Query; impl Query { pub fn default_limit() -> u32 { 20 } }",
    )
    .unwrap();
    let source = "pub struct QueryDto {
        #[serde(default = \"crate::m::Query::default_limit\")] limit: u32,
        #[serde(default = \"crate::m::Query::missing\")] missing: u32,
        #[serde(default = \"crate::nowhere::Query::default_limit\")] nowhere: u32,
        }";
    std::fs::write(src.join("api.rs"), source).unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let item: syn::ItemStruct = syn::parse_str(source).unwrap();
    let defaults = extract_field_defaults_from_path(&item.into(), &src.join("api.rs"));
    assert_eq!(defaults.get("limit"), Some(&serde_json::json!(20)));
    assert_eq!(defaults.get("missing"), None);
    assert_eq!(defaults.get("nowhere"), None);
}
