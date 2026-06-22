use super::*;
use serial_test::serial;
use std::path::Path;
use tempfile::TempDir;
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
    use syn::Type;
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
fn test_find_struct_from_path_empty_segments() {
    use syn::{Path, TypePath};
    let empty_path = Path {
        leading_colon: None,
        segments: syn::punctuated::Punctuated::new(),
    };
    let ty = Type::Path(TypePath {
        qself: None,
        path: empty_path,
    });
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
    assert!(result.is_none(), "Empty segments should return None");
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
#[serial]
fn test_find_struct_by_name_unreadable_file() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::write(
        src_dir.join("valid.rs"),
        "pub struct Target { pub id: i32 }",
    )
    .unwrap();
    let broken = src_dir.join("broken.rs");
    let nonexistent = src_dir.join("nonexistent");
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&nonexistent, &broken);
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_file(&nonexistent, &broken);
    let result = find_struct_by_name_in_all_files(src_dir, "Target", None);
    assert!(
        result.is_some(),
        "Should find Target, skipping broken symlink"
    );
}
#[test]
#[serial]
fn test_find_struct_by_name_unparseable_file() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::write(src_dir.join("broken.rs"), "this is not valid rust {{{{").unwrap();
    std::fs::write(
        src_dir.join("valid.rs"),
        "pub struct Target { pub id: i32 }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Target", None);
    assert!(
        result.is_some(),
        "Should find Target in valid file, skipping broken"
    );
}
#[test]
#[serial]
fn test_find_struct_disambiguation_with_hint() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("user.rs"),
        "pub struct Model { pub id: i32, pub name: String }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("memo.rs"),
        "pub struct Model { pub id: i32, pub title: String }",
    )
    .unwrap();
    let result_no_hint = find_struct_by_name_in_all_files(src_dir, "Model", None);
    assert!(
        result_no_hint.is_none(),
        "Without hint, multiple Models should be ambiguous"
    );
    let result_with_hint = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
    assert!(
        result_with_hint.is_some(),
        "With UserSchema hint, should find user.rs"
    );
    let (metadata, module_path) = result_with_hint.unwrap();
    assert!(
        metadata.definition.contains("name"),
        "Should be user Model with name field"
    );
    assert!(
        module_path.contains(&"user".to_string()),
        "Module path should contain 'user'"
    );
    let result_memo = find_struct_by_name_in_all_files(src_dir, "Model", Some("MemoSchema"));
    assert!(
        result_memo.is_some(),
        "With MemoSchema hint, should find memo.rs"
    );
    let (metadata_memo, _) = result_memo.unwrap();
    assert!(
        metadata_memo.definition.contains("title"),
        "Should be memo Model with title field"
    );
}
#[test]
#[serial]
fn test_find_struct_disambiguation_with_response_suffix() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("user.rs"),
        "pub struct Data { pub id: i32 }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("item.rs"),
        "pub struct Data { pub name: String }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Data", Some("UserResponse"));
    assert!(
        result.is_some(),
        "With UserResponse hint, should find user.rs"
    );
}
#[test]
#[serial]
fn test_find_struct_disambiguation_with_request_suffix() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("user.rs"),
        "pub struct Input { pub id: i32 }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("item.rs"),
        "pub struct Input { pub name: String }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Input", Some("UserRequest"));
    assert!(
        result.is_some(),
        "With UserRequest hint, should find user.rs"
    );
}
#[test]
#[serial]
fn test_find_struct_disambiguation_still_ambiguous() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("user_admin.rs"),
        "pub struct Model { pub id: i32 }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("user_regular.rs"),
        "pub struct Model { pub name: String }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
    assert!(
        result.is_none(),
        "Multiple files matching hint should still be ambiguous"
    );
}
#[test]
#[serial]
fn test_find_struct_disambiguation_snake_case_filename() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("admin_user.rs"),
        "pub struct Model { pub id: i32, pub role: String }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("regular_user.rs"),
        "pub struct Model { pub id: i32, pub name: String }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("AdminUserSchema"));
    assert!(
        result.is_some(),
        "AdminUserSchema hint should match admin_user.rs"
    );
    let (metadata, module_path) = result.unwrap();
    assert!(
        metadata.definition.contains("role"),
        "Should be admin_user Model with role field"
    );
    assert!(
        module_path.contains(&"admin_user".to_string()),
        "Module path should contain 'admin_user'"
    );
    let result_regular =
        find_struct_by_name_in_all_files(src_dir, "Model", Some("RegularUserSchema"));
    assert!(
        result_regular.is_some(),
        "RegularUserSchema hint should match regular_user.rs"
    );
    let (metadata_regular, _) = result_regular.unwrap();
    assert!(
        metadata_regular.definition.contains("name"),
        "Should be regular_user Model with name field"
    );
}
#[test]
#[serial]
fn test_find_struct_from_schema_path_empty_string() {
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    let temp_dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_struct_from_schema_path("");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_none(), "Empty path should return None");
}
#[test]
#[serial]
fn test_find_struct_from_schema_path_no_module() {
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    let temp_dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_struct_from_schema_path("crate::Schema");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_none(), "Path with no module should return None");
}
#[test]
#[serial]
fn test_find_struct_from_schema_path_with_non_struct_items() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    let content = r"
pub enum NotStruct { A, B }
pub fn not_struct() {}
pub struct Target { pub id: i32 }
pub const NOT_STRUCT: i32 = 1;
";
    std::fs::write(models_dir.join("item.rs"), content).unwrap();
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_struct_from_schema_path("crate::models::item::Target");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_some(), "Should find Target struct");
    assert!(result.unwrap().definition.contains("Target"));
}

#[test]
#[serial]
fn test_find_struct_from_schema_path_trims_segments() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::write(
        models_dir.join("item.rs"),
        "pub struct Target { pub id: i32 }",
    )
    .unwrap();
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_struct_from_schema_path("crate :: models :: item :: Target");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_some(), "Whitespace around :: should be ignored");
}

#[test]
#[serial]
fn test_find_model_from_schema_path_empty_after_filter() {
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    let temp_dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_model_from_schema_path("Schema");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_none(), "Empty segments should return None");
}
#[test]
#[serial]
fn test_find_model_from_schema_path_no_module() {
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    let temp_dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_model_from_schema_path("crate::Schema");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_none(), "No module segments should return None");
}
#[test]
#[serial]
fn test_find_model_from_schema_path_success() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    let content = "pub struct Model { pub id: i32, pub name: String }";
    std::fs::write(models_dir.join("user.rs"), content).unwrap();
    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let result = find_model_from_schema_path("crate::models::user::Schema");
    unsafe {
        if let Some(dir) = original {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }
    assert!(result.is_some(), "Should find Model");
    assert!(result.unwrap().definition.contains("Model"));
}
#[test]
#[serial]
fn test_find_struct_disambiguation_fallback_contains() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();
    std::fs::create_dir(src_dir.join("models")).unwrap();
    std::fs::write(
        src_dir.join("models").join("special_item.rs"),
        "pub struct Model { pub special_field: i32 }",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("models").join("regular.rs"),
        "pub struct Model { pub regular_field: String }",
    )
    .unwrap();
    let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("SpecialSchema"));
    assert!(
        result.is_some(),
        "SpecialSchema hint should match special_item.rs via contains fallback"
    );
    let (metadata, module_path) = result.unwrap();
    assert!(
        metadata.definition.contains("special_field"),
        "Should be special_item Model with special_field"
    );
    assert!(
        module_path.contains(&"special_item".to_string()),
        "Module path should contain 'special_item'"
    );
}
