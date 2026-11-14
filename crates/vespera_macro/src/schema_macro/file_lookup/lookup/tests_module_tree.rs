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
/// module; the file lookup wins, storage stays the fallback.
#[rstest]
#[case::other_module_uses_file("crate::models::user::User", "models/mod.rs", "pub id")]
#[case::same_module_uses_storage("crate::models::user::User", "models/user.rs", "pub stored")]
#[case::missing_file_falls_back("crate::nowhere::User", "models/mod.rs", "pub stored")]
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
