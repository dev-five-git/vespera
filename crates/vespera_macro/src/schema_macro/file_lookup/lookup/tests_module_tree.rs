//! Sources outside the `src/<module>.rs` convention: crate roots, inline
//! modules, `#[path]` modules and call-site relative (`self::` / `super::`)
//! paths.

use std::path::{Path, PathBuf};

use rstest::rstest;
use tempfile::TempDir;

use super::*;
use crate::schema_macro::file_cache::bump_epoch;

const TREE: &str = r#"
pub struct RootModel { pub id: i32 }
#[path = "custom/place.rs"]
pub mod placed;
pub mod models;
pub mod inline {
    pub struct InlineModel { pub id: i32 }
    pub mod deeper {
        pub struct DeepModel { pub id: i32 }
    }
}
"#;

const FILES: &[(&str, &str)] = &[
    ("custom/place.rs", "pub struct PlacedModel { pub id: i32 }"),
    (
        "models/mod.rs",
        "pub mod user;\npub struct ModsModel { pub id: i32 }",
    ),
    (
        "models/user.rs",
        "pub struct User { pub id: i32 }\nmod hidden { pub struct Hidden { pub id: i32 } }",
    ),
];

fn temp_crate(root: &str) -> (TempDir, crate::env_access::test_env::EnvGuard) {
    let dir = TempDir::new().unwrap();
    for (path, content) in std::iter::once(&(root, TREE)).chain(FILES) {
        let path = dir.path().join("src").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();
    (dir, guard)
}

fn lookup(
    path: &str,
    call_site: Option<&Path>,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    let ty: syn::Type = syn::parse_str(path).unwrap();
    find_struct_with_call_site(&ty, call_site)
}

#[rstest]
#[case::crate_root("crate::RootModel", "RootModel", "crate")]
#[case::crate_root_inline("crate::inline::InlineModel", "InlineModel", "crate::inline")]
#[case::inline_nested("inline::deeper::DeepModel", "DeepModel", "crate::inline::deeper")]
#[case::path_attribute("crate::placed::PlacedModel", "PlacedModel", "crate::placed")]
#[case::mod_rs("models::ModsModel", "ModsModel", "crate::models")]
#[case::plain_file("crate::models::user::User", "User", "crate::models::user")]
fn qualified_lookup_walks_module_tree(
    #[case] path: &str,
    #[case] name: &str,
    #[case] module: &str,
    #[values("lib.rs", "main.rs")] root: &str,
) {
    let (_dir, _guard) = temp_crate(root);
    let (metadata, module_path) = lookup(path, None).unwrap();
    assert_eq!(metadata.name, name);
    assert!(metadata.definition.contains(&format!("struct {name}")));
    assert_eq!(module_path.join("::"), module);
}

#[rstest]
#[case::self_inline(
    "self::hidden::Hidden",
    "models/user.rs",
    "crate::models::user::hidden"
)]
#[case::super_sibling("super::user::User", "models/user.rs", "crate::models::user")]
#[case::super_past_root("super::super::RootModel", "models/mod.rs", "crate")]
#[case::self_from_root("self::inline::InlineModel", "lib.rs", "crate::inline")]
fn relative_lookup_uses_call_site_module(
    #[case] path: &str,
    #[case] call_site: &str,
    #[case] module: &str,
) {
    let (dir, _guard) = temp_crate("lib.rs");
    let call_site: PathBuf = dir.path().join("src").join(call_site);
    let (_, module_path) = lookup(path, Some(&call_site)).unwrap();
    assert_eq!(module_path.join("::"), module);
}

/// M15: a first segment the call-site module imports resolves through
/// that import (it used to be read from the crate root: `crate::user`).
#[test]
fn path_through_call_site_import() {
    let (dir, _guard) = temp_crate("lib.rs");
    let call_site = dir.path().join("src/handlers.rs");
    std::fs::write(
        &call_site,
        "use crate::models::user;\nuse crate::models as m;",
    )
    .unwrap();
    bump_epoch();
    let (metadata, module_path) = lookup("user::User", Some(&call_site)).unwrap();
    assert_eq!(
        (metadata.name.as_str(), module_path.join("::").as_str()),
        ("User", "crate::models::user")
    );
    let (_, module_path) = lookup("m::user::User", Some(&call_site)).unwrap();
    assert_eq!(module_path.join("::"), "crate::models::user");
    // Not imported: crate-root relative, as before.
    let (_, module_path) = lookup("models::ModsModel", Some(&call_site)).unwrap();
    assert_eq!(module_path.join("::"), "crate::models");
}

/// L17: in an integration test, `crate::` is the test crate (`tests/`), not
/// the library's `src/`.
#[test]
fn integration_test_crate_root() {
    let (dir, _guard) = temp_crate("lib.rs");
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(tests.join("common")).unwrap();
    std::fs::write(tests.join("api.rs"), "mod common;").unwrap();
    std::fs::write(
        tests.join("common/mod.rs"),
        "pub struct Shared { pub id: i32 }",
    )
    .unwrap();
    let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "api");
    bump_epoch();
    let call_site = tests.join("api.rs");
    let (metadata, module_path) = lookup("crate::common::Shared", Some(&call_site)).unwrap();
    assert_eq!(
        (metadata.name.as_str(), module_path.join("::").as_str()),
        ("Shared", "crate::common")
    );
    // From the helper itself: its module is `crate::common`.
    let (_, module_path) = lookup("Shared", Some(&tests.join("common/mod.rs"))).unwrap();
    assert_eq!(module_path.join("::"), "crate::common");
}

#[test]
fn relative_lookup_without_call_site_is_crate_relative() {
    let (_dir, _guard) = temp_crate("lib.rs");
    let (_, module_path) = lookup("self::models::ModsModel", None).unwrap();
    assert_eq!(module_path.join("::"), "crate::models");
}

#[test]
fn bare_lookup_finds_unique_inline_struct() {
    let (dir, _guard) = temp_crate("lib.rs");
    let call_site = dir.path().join("src/models/user.rs");
    let (metadata, module_path) = lookup("Hidden", Some(&call_site)).unwrap();
    assert_eq!(metadata.name, "Hidden");
    assert_eq!(module_path.join("::"), "crate::models::user::hidden");
    assert!(matches!(
        lookup("Missing", Some(&call_site)),
        Err(LookupError::BareNotFound { .. })
    ));
    assert!(matches!(
        lookup("Hidden", None),
        Err(LookupError::BareNotFound { .. })
    ));
}

#[rstest]
#[case::crate_root("crate::Missing", 2)]
#[case::module("models::Missing", 4)]
fn missing_struct_lists_module_files_and_crate_roots(#[case] path: &str, #[case] searched: usize) {
    let (_dir, _guard) = temp_crate("lib.rs");
    let Err(LookupError::NotFound {
        searched: files, ..
    }) = lookup(path, None)
    else {
        panic!("expected NotFound");
    };
    assert_eq!(files.len(), searched);
}

#[rstest]
#[case::main("src/main.rs", "crate")]
#[case::bin_file("src/bin/tool.rs", "crate")]
#[case::bin_dir("src/bin/tool/main.rs", "crate")]
#[case::bin_dir_module("src/bin/tool/other.rs", "crate::bin::tool::other")]
#[case::nested_main("src/models/main.rs", "crate::models::main")]
fn crate_root_files_map_to_crate(#[case] file: &str, #[case] module: &str) {
    let module_path = file_path_to_module_path(Path::new(file), Path::new("src"));
    assert_eq!(module_path.join("::"), module);
}

fn stored_user(dir: &TempDir, file: &str) -> StructMetadata {
    let file = dir.path().join("src").join(file);
    StructMetadata::new(
        "User".to_string(),
        "pub struct User { pub stored: i32 }".to_string(),
    )
    .with_source_identity(format!("{}::User@1:11", file.display()))
}

#[test]
fn storage_module_path_reads_identity_file() {
    let (dir, _guard) = temp_crate("lib.rs");
    assert_eq!(
        storage_module_path(&stored_user(&dir, "models/user.rs"))
            .unwrap()
            .join("::"),
        "crate::models::user"
    );
    let plain = StructMetadata::new("User".to_string(), String::new());
    assert!(storage_module_path(&plain).is_none());
    let outside = plain
        .clone()
        .with_source_identity("/elsewhere/x.rs::User@1:1".to_string());
    assert!(storage_module_path(&outside).is_none());
    let malformed = plain.with_source_identity("no-at-sign".to_string());
    assert!(storage_module_path(&malformed).is_none());
}

/// A storage entry declared in an inline `mod` belongs to that module: the
/// identity's ident (and its line, when the name repeats) finds the chain.
#[rstest]
#[case::inline_mod("models/user.rs", "Hidden@2:31", "crate::models::user::hidden")]
#[case::file_top_level("models/user.rs", "User@1:11", "crate::models::user")]
#[case::nested_inline("lib.rs", "DeepModel@9:19", "crate::inline::deeper")]
#[case::unknown_ident("lib.rs", "Missing@1:1", "crate")]
#[case::unparseable_line("lib.rs", "InlineModel@x:1", "crate::inline")]
fn storage_module_path_follows_inline_modules(
    #[case] file: &str,
    #[case] at: &str,
    #[case] expected: &str,
) {
    let (dir, _guard) = temp_crate("lib.rs");
    let file = dir.path().join("src").join(file);
    let stored = StructMetadata::new("X".to_string(), String::new())
        .with_source_identity(format!("{}::{at}", file.display()));
    assert_eq!(storage_module_path(&stored).unwrap().join("::"), expected);
}

#[rstest]
#[case::bare("User", None)]
#[case::qualified("models::user::User", Some("crate::models::user"))]
#[case::crate_qualified("crate::models::User", Some("crate::models"))]
fn requested_module_path_absolutizes(#[case] path: &str, #[case] expected: Option<&str>) {
    let ty: syn::Type = syn::parse_str(path).unwrap();
    assert_eq!(
        requested_module_path(&ty).map(|m| m.join("::")),
        expected.map(str::to_string)
    );
    assert!(requested_module_path(&syn::parse_str("(i32, i32)").unwrap()).is_none());
}

/// Storage keyed by bare name must not answer a qualified path into another
/// module: the file lookup wins.
#[rstest]
#[case::other_module_uses_file("crate::models::user::User", "models/mod.rs", "pub id")]
#[case::same_module_uses_storage("crate::models::user::User", "models/user.rs", "pub stored")]
#[case::bare_uses_storage("User", "models/mod.rs", "pub stored")]
fn storage_hit_must_match_requested_module(
    #[case] source: &str,
    #[case] stored_in: &str,
    #[case] expected_field: &str,
) {
    let (dir, _guard) = temp_crate("lib.rs");
    let storage =
        std::collections::HashMap::from([("User".to_string(), stored_user(&dir, stored_in))]);
    let input: crate::schema_macro::SchemaTypeInput =
        syn::parse_str(&format!("UserDto from {source}")).unwrap();
    let (tokens, _) = crate::schema_macro::generate_schema_type_code(&input, &storage).unwrap();
    let rendered = tokens.to_string();
    assert!(rendered.contains(expected_field), "{rendered}");
}

/// M15: a qualified path the file lookup cannot find is an error, also when
/// a same-named schema of another module is stored (it used to be used).
#[rstest]
#[case::other_module_stored(
    Some("models/mod.rs"),
    "note: `User` exists in `crate::models`, which is not the requested module"
)]
#[case::nothing_stored(None, "")]
fn missing_qualified_source_never_falls_back(#[case] stored_in: Option<&str>, #[case] note: &str) {
    let (dir, _guard) = temp_crate("lib.rs");
    let storage: std::collections::HashMap<_, _> = stored_in
        .map(|file| ("User".to_string(), stored_user(&dir, file)))
        .into_iter()
        .collect();
    let input: crate::schema_macro::SchemaTypeInput =
        syn::parse_str("UserDto from crate::nowhere::User").unwrap();
    let error = crate::schema_macro::generate_schema_type_code(&input, &storage).unwrap_err();
    let messages: Vec<String> = error.into_iter().map(|e| e.to_string()).collect();
    assert!(messages[0].contains("User"), "{messages:?}");
    assert_eq!(messages.get(1).map_or("", String::as_str), note);
}

/// A crate whose `models` module re-exports `user::User` and whose
/// `user.rs` imports the field type `Money` from `crate::common`.
fn reexporting_crate() -> (TempDir, crate::env_access::test_env::EnvGuard) {
    let dir = TempDir::new().unwrap();
    for (path, content) in [
        ("lib.rs", "pub mod common;\npub mod models;"),
        ("common.rs", "pub struct Money(pub i64);"),
        ("models/mod.rs", "pub mod user;\npub use user::User;"),
        (
            "models/user.rs",
            "use crate::common::Money;\npub struct User { pub price: Money }",
        ),
    ] {
        let path = dir.path().join("src").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();
    (dir, guard)
}

fn stored_priced_user(dir: &TempDir) -> StructMetadata {
    let file = dir.path().join("src/models/user.rs");
    StructMetadata::new(
        "User".to_string(),
        "pub struct User { pub price: Money }".to_string(),
    )
    .with_source_identity(format!("{}::User@2:12", file.display()))
}

/// MED-1 / MED-2: a trusted storage hit takes its defining module, not the
/// path as written (`models::user::Money` names a private import), and a
/// path through a `pub use` re-export names the same item - with and
/// without a storage entry.
#[rstest]
#[case::relative_path_storage("models::user::User", true)]
#[case::reexport_storage("crate::models::User", true)]
#[case::reexport_file_lookup("crate::models::User", false)]
fn field_types_resolve_from_the_defining_module(#[case] source: &str, #[case] stored: bool) {
    let (dir, _guard) = reexporting_crate();
    let storage: std::collections::HashMap<_, _> = stored
        .then(|| ("User".to_string(), stored_priced_user(&dir)))
        .into_iter()
        .collect();
    let input: crate::schema_macro::SchemaTypeInput =
        syn::parse_str(&format!("UserDto from {source}")).unwrap();
    let (tokens, _) = crate::schema_macro::generate_schema_type_code(&input, &storage).unwrap();
    let rendered = tokens.to_string();
    assert!(
        rendered.contains("price : crate :: common :: Money"),
        "{rendered}"
    );
}

/// T-1: of cfg twins the lookup takes the one the build compiles, and a
/// `#[cfg(any())]` struct (or inline module) is no definition at all.
#[rstest]
#[case::pg_enabled("--cfg\nfeature=\"pg\"", "pub big")]
#[case::pg_disabled("--cfg\nfeature=\"other\"", "pub small")]
fn cfg_inactive_structs_are_skipped(#[case] cfg: &str, #[case] expected: &str) {
    let dir = TempDir::new().unwrap();
    let user = dir.path().join("src/models/user.rs");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(
        &user,
        "#[cfg(feature = \"pg\")] pub struct User { pub big: i64 }\n\
         #[cfg(not(feature = \"pg\"))] pub struct User { pub small: i32 }\n\
         #[cfg(any())] pub struct Dead { pub id: i32 }\n\
         #[cfg(any())] mod gone { pub struct Gone { pub id: i32 } }",
    )
    .unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let _args = crate::env_access::test_env::set_var(
        crate::env_access::COMPILER_ARGS_KEY,
        format!("rustc\n--crate-name\napp\n{cfg}"),
    );
    bump_epoch();
    let (metadata, _) = lookup("crate::models::user::User", None).unwrap();
    assert!(
        metadata.definition.contains(expected),
        "{}",
        metadata.definition
    );
    assert!(matches!(
        lookup("crate::models::user::Dead", None),
        Err(LookupError::NotFound { .. })
    ));
    assert!(matches!(
        lookup("Gone", Some(&user)),
        Err(LookupError::BareNotFound { .. })
    ));
    let items = syn::parse_file(&std::fs::read_to_string(&user).unwrap())
        .unwrap()
        .items;
    let in_items = crate::schema_macro::module_scope::struct_in_items(&items, "User").unwrap();
    assert!(in_items.contains(expected), "{in_items}");
}

#[test]
fn reexported_module_follows_crate_imports_only() {
    let (_dir, _guard) = reexporting_crate();
    let module = |path: &str| path.split("::").map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
        reexported_module(&module("crate::models"), "User"),
        Some(module("crate::models::user"))
    );
    // Defined there (no re-export) and unknown names stay put.
    assert_eq!(
        reexported_module(&module("crate::models::user"), "User"),
        None
    );
    assert_eq!(reexported_module(&module("crate::models"), "Missing"), None);
}

/// A bare source name: the call-site file's own definition is preferred
/// over a storage entry derived in another file; an entry of the call-site
/// file itself, one without a (well-formed) identity, or a name the file does
/// not define keeps the storage entry.
#[test]
fn call_site_definition_prefers_the_local_struct() {
    let (dir, _guard) = temp_crate("lib.rs");
    let lib = dir.path().join("src/lib.rs");
    let stored = |identity: Option<String>| StructMetadata {
        source_identity: identity,
        ..StructMetadata::new("RootModel".to_string(), String::new())
    };
    let ty: syn::Type = syn::parse_str("RootModel").unwrap();
    let other = format!(
        "{}::RootModel@1:1",
        dir.path().join("src/other.rs").display()
    );
    let (found, module) = call_site_definition_in(&stored(Some(other.clone())), &ty, &lib)
        .expect("the call-site file defines RootModel");
    assert!(
        found.definition.contains("RootModel"),
        "{}",
        found.definition
    );
    assert_eq!(module, ["crate"]);
    let own = format!("{}::RootModel@2:1", lib.display());
    assert!(call_site_definition_in(&stored(Some(own)), &ty, &lib).is_none());
    assert!(call_site_definition_in(&stored(None), &ty, &lib).is_none());
    assert!(call_site_definition_in(&stored(Some("no-position".into())), &ty, &lib).is_none());
    assert!(call_site_definition_in(&stored(Some("file@1:1".into())), &ty, &lib).is_none());
    let missing: syn::Type = syn::parse_str("Missing").unwrap();
    assert!(call_site_definition_in(&stored(Some(other)), &missing, &lib).is_none());
    // Without a call-site file (unit tests) nothing is local.
    assert!(call_site_definition(&stored(None), &ty, None).is_none());
}
