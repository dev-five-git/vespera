use std::path::Path;

use serial_test::serial;
use syn::Type;
use tempfile::TempDir;

use super::*;
use crate::schema_macro::file_cache::{bump_epoch, inject_struct_definition_for_test};

struct RestoreManifest(Option<String>);

impl Drop for RestoreManifest {
    fn drop(&mut self) {
        // SAFETY: environment-mutating tests are serialized and restore their prior value.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                None => std::env::remove_var("CARGO_MANIFEST_DIR"),
            }
        }
    }
}

#[test]
fn bare_struct_file_lookup_returns_metadata_and_module_path() {
    let temp = TempDir::new().unwrap();
    let src_dir = temp.path().join("src");
    let model_dir = src_dir.join("models");
    std::fs::create_dir_all(&model_dir).unwrap();
    let file_path = model_dir.join("user.rs");
    std::fs::write(&file_path, "pub struct User { pub id: i32 }").unwrap();

    let (metadata, module_path) = find_bare_struct_in_file(&file_path, "User", &src_dir).unwrap();

    assert_eq!(metadata.name, "User");
    assert!(metadata.definition.contains("pub struct User"));
    assert_eq!(module_path, ["crate", "models", "user"]);
}

#[test]
fn bare_struct_file_lookup_reports_missing_struct() {
    let temp = TempDir::new().unwrap();
    let src_dir = temp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("models.rs");
    std::fs::write(&file_path, "pub struct Other;").unwrap();

    let result = find_bare_struct_in_file(&file_path, "Missing", &src_dir);

    assert!(matches!(
        result,
        Err(LookupError::BareNotFound { struct_name }) if struct_name == "Missing"
    ));
}

#[test]
fn collect_rs_entry_classifies_rust_files_directly() {
    let temp = TempDir::new().unwrap();
    let rust_file = temp.path().join("model.rs");
    let text_file = temp.path().join("README.md");
    std::fs::write(&rust_file, "pub struct Model;").unwrap();
    std::fs::write(&text_file, "fixture").unwrap();
    let mut entries: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    entries.sort_by_key(std::fs::DirEntry::path);
    let mut files = Vec::new();

    for entry in entries {
        collect_rs_entry(&entry, entry.file_type().unwrap(), &mut files);
    }

    assert_eq!(files, vec![rust_file]);
}

#[test]
fn test_file_path_to_module_path_simple() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    let file_path = src_dir.join("models").join("user.rs");
    let result = file_path_to_module_path(&file_path, src_dir);
    assert_eq!(result, vec!["crate", "models", "user"]);
}

#[test]
fn test_file_path_to_module_path_mod_rs() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    let file_path = src_dir.join("models").join("mod.rs");
    let result = file_path_to_module_path(&file_path, src_dir);
    assert_eq!(result, vec!["crate", "models"]);
}

#[test]
fn test_file_path_to_module_path_lib_rs() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    let file_path = src_dir.join("lib.rs");
    let result = file_path_to_module_path(&file_path, src_dir);
    assert_eq!(result, vec!["crate"]);
}

#[test]
fn test_file_path_to_module_path_not_under_src() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let file_path = temp_dir.path().join("other").join("file.rs");
    let result = file_path_to_module_path(&file_path, &src_dir);
    assert_eq!(result, vec!["crate"]);
}

#[test]
fn test_collect_rs_files_recursive_empty_dir() {
    let temp_dir = TempDir::new().unwrap();
    let mut files = Vec::new();
    collect_rs_files_recursive(temp_dir.path(), &mut files);
    assert!(files.is_empty());
}

#[test]
fn test_collect_rs_files_recursive_nonexistent_dir() {
    let mut files = Vec::new();
    collect_rs_files_recursive(Path::new("/nonexistent/path"), &mut files);
    assert!(files.is_empty());
}

#[test]
fn test_collect_rs_files_recursive_with_files() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
    std::fs::create_dir(temp_dir.path().join("models")).unwrap();
    std::fs::write(
        temp_dir.path().join("models").join("user.rs"),
        "struct User;",
    )
    .unwrap();
    std::fs::write(temp_dir.path().join("other.txt"), "not a rust file").unwrap();
    let mut files = Vec::new();
    collect_rs_files_recursive(temp_dir.path(), &mut files);
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.extension().unwrap() == "rs"));
}

#[test]
#[serial]
fn test_find_struct_from_path_non_path_type() {
    let ty: Type = syn::parse_str("&str").unwrap();
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    let temp_dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_struct_from_path(&ty, None);
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_none(), "Non-path type should return None");
}

#[test]
#[serial]
fn test_find_struct_from_path_file_with_non_matching_items() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    let content = r"
pub enum SomeEnum { A, B }
pub fn some_function() {}
pub const SOME_CONST: i32 = 42;
pub trait SomeTrait {}
pub struct NotTarget { pub x: i32 }
pub struct Target { pub id: i32 }
";
    std::fs::write(models_dir.join("mixed.rs"), content).unwrap();
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let ty: Type = syn::parse_str("crate::models::mixed::Target").unwrap();
    let result = find_struct_from_path(&ty, None);
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_some(), "Should find Target struct");
    let (metadata, _) = result.unwrap();
    assert!(metadata.definition.contains("Target"));
}

#[test]
fn lookup_errors_render_each_diagnostic_and_empty_search_set() {
    let ty: Type = syn::parse_str("Model").unwrap();
    assert!(
        LookupError::InvalidTypePath
            .to_syn_error(&ty)
            .to_string()
            .contains("must be a type path")
    );
    assert!(
        LookupError::MissingManifestDir
            .to_syn_error(&ty)
            .to_string()
            .contains("CARGO_MANIFEST_DIR is not set")
    );
    assert_eq!(render_paths(&[]), "<none>");
}

#[test]
#[serial]
fn detailed_lookup_reports_missing_manifest_and_invalid_type_path() {
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") };
    bump_epoch();
    let path_ty: Type = syn::parse_str("Model").unwrap();
    assert!(matches!(
        find_struct_from_path_detailed(&path_ty, None),
        Err(LookupError::MissingManifestDir)
    ));

    let temp = TempDir::new().unwrap();
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };
    bump_epoch();
    let reference: Type = syn::parse_str("&str").unwrap();
    assert!(matches!(
        find_struct_from_path_detailed(&reference, None),
        Err(LookupError::InvalidTypePath)
    ));
}

#[test]
#[serial]
fn detailed_lookup_rejects_empty_type_path() {
    let temp = TempDir::new().unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };
    bump_epoch();
    let ty = Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        },
    });
    assert!(matches!(
        find_struct_from_path_detailed(&ty, None),
        Err(LookupError::InvalidTypePath)
    ));
}

#[test]
fn bare_lookup_uses_injected_call_site_definition_when_local_file_is_available() {
    let Some(call_site_file) = proc_macro2::Span::call_site().local_file() else {
        return;
    };
    inject_struct_definition_for_test(
        &call_site_file,
        "InjectedCallSiteModel",
        "pub struct InjectedCallSiteModel { pub id: i32 }",
    );
    let src_dir = call_site_file
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "src"))
        .expect("this test module lives below src");

    let (metadata, module_path) = find_bare_struct_in_call_site(src_dir, "InjectedCallSiteModel")
        .expect("injected call-site definition resolves");
    assert_eq!(metadata.name, "InjectedCallSiteModel");
    assert!(module_path.starts_with(&["crate".to_string()]));
}

#[test]
fn module_path_falls_back_to_components_after_src() {
    let file = Path::new("unrelated/root/src/models/user/mod.rs");
    let unrelated_src = Path::new("different/src");
    assert_eq!(
        file_path_to_module_path(file, unrelated_src),
        ["crate", "models", "user"]
    );
}

#[test]
#[serial]
fn string_schema_and_model_lookups_reject_empty_paths() {
    let temp = TempDir::new().unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };
    bump_epoch();

    assert!(find_struct_from_schema_path(":: ::").is_none());
    assert!(find_model_from_schema_path("Schema").is_none());
    assert!(find_model_from_schema_path("crate::Schema").is_none());
}
