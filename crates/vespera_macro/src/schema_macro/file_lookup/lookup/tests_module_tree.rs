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
    find_struct_with_call_site(&ty, call_site, 0)
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

/// A `schema_type!` inside an inline `mod` resolves `super::` / `self::`,
/// that module's imports and bare names in THAT module, not the file's top
/// level (`line` is the macro call's line).
#[rstest]
#[case::super_from_inline("super::Model", 9, "a", "crate::models::user")]
#[case::bare_in_inline("Model", 4, "b", "crate::models::user::v2")]
#[case::bare_imported_in_inline("Model", 8, "a", "crate::models::user")]
// L8 (round 13): an aliased import (`use super::Model as User;`) names the
// struct it imports; it required the alias to be the struct's own name.
#[case::bare_aliased_in_inline("User", 13, "a", "crate::models::user")]
#[case::self_in_inline("self::Model", 4, "b", "crate::models::user::v2")]
#[case::top_level("Model", 1, "a", "crate::models::user")]
fn lookup_inside_an_inline_module(
    #[case] path: &str,
    #[case] line: usize,
    #[case] field: &str,
    #[case] module: &str,
) {
    let (dir, _guard) = temp_crate("lib.rs");
    let call_site = dir.path().join("src/models/user.rs");
    std::fs::write(
        &call_site,
        "pub struct Model { pub a: i32 }\n\
         pub mod v2 {\n\
             pub struct Model { pub b: i32 }\n\
             // schema_type!(D from Model);\n\
         }\n\
         pub mod dto {\n\
             use super::Model;\n\
             // schema_type!(D from Model);\n\
             // schema_type!(D from super::Model);\n\
         }\n\
         pub mod alias {\n\
             use super::Model as User;\n\
             // schema_type!(D from User);\n\
         }\n",
    )
    .unwrap();
    bump_epoch();
    let ty: syn::Type = syn::parse_str(path).unwrap();
    let (metadata, module_path) = find_struct_with_call_site(&ty, Some(&call_site), line).unwrap();
    assert!(
        metadata.definition.contains(&format!("pub {field} :")),
        "{}",
        metadata.definition
    );
    assert_eq!(module_path.join("::"), module);
    // A bare name neither defined nor imported in the inline module.
    let missing: syn::Type = syn::parse_str("Hidden").unwrap();
    assert!(matches!(
        find_struct_with_call_site(&missing, Some(&call_site), 4),
        Err(LookupError::BareNotFound { .. })
    ));
}

/// A module path through an import of ANOTHER crate (`use entity::user;`)
/// is a clear error, not a lookup of `crate::user` in this crate.
#[test]
fn path_through_external_crate_import_is_an_error() {
    let (dir, _guard) = temp_crate("lib.rs");
    let call_site = dir.path().join("src/handlers.rs");
    std::fs::write(&call_site, "use entity::user;").unwrap();
    bump_epoch();
    let Err(err) = lookup("user::Model", Some(&call_site)) else {
        panic!("an external-crate source must be rejected");
    };
    assert!(matches!(&err, LookupError::ExternalCrate { path } if path == "entity::user"));
    let message = err.to_syn_error(&quote::quote!(x)).to_string();
    assert!(message.contains("another crate"), "{message}");
}

/// L3 (round 12): a path written through a dependency crate the call site
/// never imports (`entity::user::Model`) is that crate's, not a missing
/// `src/entity/user.rs`: it used to be a NotFound listing local files.
#[rstest]
#[case::plain_extern("--extern\nentity=/deps/libentity.rlib", true)]
#[case::extern_option("--extern\npriv:entity=/deps/libentity.rlib", true)]
#[case::other_extern("--extern\nserde=/deps/libserde.rlib", false)]
#[case::not_rustc("", false)]
fn unimported_dependency_path_is_external(#[case] args: &str, #[case] external: bool) {
    let (_dir, _guard) = temp_crate("lib.rs");
    let _args = crate::env_access::test_env::set_var(
        crate::env_access::COMPILER_ARGS_KEY,
        format!("rustc\n--crate-name\napp\n{args}"),
    );
    let err = lookup("entity::user::Model", None).unwrap_err();
    assert_eq!(
        matches!(&err, LookupError::ExternalCrate { path } if path == "entity::user"),
        external,
        "{err:?}"
    );
    assert_eq!(matches!(err, LookupError::NotFound { .. }), !external);
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
        requested_module_path(&ty).unwrap().map(|m| m.join("::")),
        expected.map(str::to_string)
    );
    assert!(
        requested_module_path(&syn::parse_str("(i32, i32)").unwrap())
            .unwrap()
            .is_none()
    );
    // Unit tests have no call-site file.
    assert!(call_site_module().is_none());
}

/// M4 (round 13): a source path through an import of another crate
/// (`use entity::types;` -> `types::User`) is an error, not a bare name: a
/// bare name let a same-named local `#[derive(Schema)]` win silently.
#[test]
fn requested_module_path_through_external_import_is_an_error() {
    let (dir, _guard) = temp_crate("lib.rs");
    std::fs::write(dir.path().join("src/handlers.rs"), "use entity::types;").unwrap();
    bump_epoch();
    let module = ["crate", "handlers"].map(String::from);
    let ty: syn::Type = syn::parse_str("types::User").unwrap();
    assert!(matches!(
        requested_module_path_in(&ty, Some(&module)),
        Err(LookupError::ExternalCrate { path }) if path == "entity::types"
    ));
    let local: syn::Type = syn::parse_str("models::User").unwrap();
    assert_eq!(
        requested_module_path_in(&local, Some(&module)).unwrap(),
        Some(vec!["crate".to_string(), "models".to_string()])
    );
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
        (
            "models/mod.rs",
            "pub mod user;\npub use user::User;\npub use user::Model as Account;",
        ),
        (
            "models/user.rs",
            "use crate::common::Money;\npub struct User { pub price: Money }\npub struct Model { pub owner: Money }",
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

/// MED-2 (round 12): a parent declaring the module with `#[path]` (one cfg
/// twin of it) or inline overrides `src/<module>.rs`: the fast path used to
/// read the plain file whatever the declaration said.
#[rstest]
#[case::redirected_twin_on("--cfg\nfeature=\"v2\"", "crate::models::Model", "pub v2")]
#[case::redirected_twin_off("--cfg\nfeature=\"other\"", "crate::models::Model", "pub v1")]
#[case::nested_redirect("--cfg\nfeature=\"v2\"", "crate::models::user::User", "pub v2_user")]
#[case::inline_parent("--cfg\nfeature=\"other\"", "crate::shadow::Model", "pub inline")]
fn parent_declarations_override_the_module_file(
    #[case] cfg: &str,
    #[case] path: &str,
    #[case] expected: &str,
) {
    let dir = TempDir::new().unwrap();
    let files = [
        (
            "lib.rs",
            "#[cfg(feature = \"v2\")] #[path = \"models_v2.rs\"] pub mod models;\n\
             #[cfg(not(feature = \"v2\"))] pub mod models;\n\
             pub mod shadow { pub struct Model { pub inline: i32 } }",
        ),
        (
            "models.rs",
            "pub mod user;\npub struct Model { pub v1: i32 }",
        ),
        ("models/user.rs", "pub struct User { pub v1_user: i32 }"),
        (
            "models_v2.rs",
            "pub mod user;\npub struct Model { pub v2: i32 }",
        ),
        // A `#[path]` file is mod-rs like: its `mod user;` is `src/user.rs`.
        ("user.rs", "pub struct User { pub v2_user: i32 }"),
        ("shadow.rs", "pub struct Model { pub stale: i32 }"),
    ];
    for (file, content) in files {
        let file = dir.path().join("src").join(file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let _args = crate::env_access::test_env::set_var(
        crate::env_access::COMPILER_ARGS_KEY,
        format!("rustc\n--crate-name\napp\n{cfg}"),
    );
    bump_epoch();
    let (metadata, _) = lookup(path, None).unwrap();
    assert!(
        metadata.definition.contains(expected),
        "{}",
        metadata.definition
    );
    let module: Vec<String> = path
        .rsplit_once("::")
        .unwrap()
        .0
        .split("::")
        .map(String::from)
        .collect();
    let items = crate::schema_macro::module_scope::module_items(&module).unwrap();
    let name = path.rsplit("::").next().unwrap();
    let in_items = crate::schema_macro::module_scope::struct_in_items(&items, name).unwrap();
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
    // L4 (round 12): an `as` alias is followed to the item it renames.
    assert_eq!(reexported_module(&module("crate::models"), "Account"), None);
    assert_eq!(
        reexported_item(&module("crate::models"), "Account"),
        Some((module("crate::models::user"), "Model".to_string()))
    );
    let (metadata, defined_in) = lookup("crate::models::Account", None).unwrap();
    assert!(
        metadata.definition.contains("owner"),
        "{}",
        metadata.definition
    );
    assert_eq!(defined_in.join("::"), "crate::models::user");
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
    assert!(call_site_definition_in(&stored(Some(other.clone())), &missing, &lib).is_none());
    // Without a call-site file (unit tests) nothing is local, even for an
    // entry of another file that the call site would define.
    assert!(call_site_definition(&stored(Some(other)), &ty, None).is_none());
}
