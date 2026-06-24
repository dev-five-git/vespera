use std::path::Path;

use serial_test::serial;
use syn::Type;
use tempfile::TempDir;

use super::*;

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
