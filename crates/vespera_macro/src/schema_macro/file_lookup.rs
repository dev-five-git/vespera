//! File system operations for finding struct definitions
//!
//! Provides functions to locate struct definitions in source files.

use std::path::Path;

use crate::metadata::StructMetadata;
use syn::Type;

/// Try to find a struct definition from a module path by reading source files.
///
/// This allows schema_type! to work with structs defined in other files, like:
/// ```ignore
/// // In src/routes/memos.rs
/// schema_type!(CreateMemoRequest from models::memo::Model, pick = ["title", "content"]);
/// ```
///
/// The function will:
/// 1. Parse the path (e.g., `models::memo::Model` or `crate::models::memo::Model`)
/// 2. Convert to file path (e.g., `src/models/memo.rs`)
/// 3. Read and parse the file to find the struct definition
///
/// For simple names (e.g., just `Model` without module path), it will scan all `.rs`
/// files in `src/` to find the struct. This supports same-file usage like:
/// ```ignore
/// pub struct Model { ... }
/// vespera::schema_type!(Schema from Model, name = "UserSchema");
/// ```
///
/// The `schema_name_hint` is used to disambiguate when multiple structs with the same
/// name exist. For example, with `name = "UserSchema"`, it will prefer `user.rs`.
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the module path.
/// For qualified paths, this is extracted from the type itself.
/// For simple names, it's inferred from the file location.
pub fn find_struct_from_path(
    ty: &Type,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Extract path segments from the type
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // The last segment is the struct name
    let struct_name = segments.last()?.clone();

    // Build possible file paths from the module path
    // e.g., models::memo::Model -> src/models/memo.rs or src/models/memo/mod.rs
    // e.g., crate::models::memo::Model -> src/models/memo.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
        .map(|s| s.as_str())
        .collect();

    // If no module path (simple name like `Model`), scan all files with schema_name hint
    if module_segments.is_empty() {
        return find_struct_by_name_in_all_files(&src_dir, &struct_name, schema_name_hint);
    }

    // For qualified paths, the module path is extracted from the type itself
    // e.g., crate::models::memo::Model -> ["crate", "models", "memo"]
    let type_module_path: Vec<String> = segments[..segments.len() - 1].to_vec();

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some((
                        StructMetadata::new_model(
                            struct_name.clone(),
                            quote::quote!(#struct_item).to_string(),
                        ),
                        type_module_path,
                    ));
                }
                _ => continue,
            }
        }
    }

    None
}

/// Find a struct by name by scanning all `.rs` files in the src directory.
///
/// This is used as a fallback when the type path doesn't include module information
/// (e.g., just `Model` instead of `crate::models::user::Model`).
///
/// Resolution strategy:
/// 1. If exactly one struct with the name exists -> use it
/// 2. If multiple exist and schema_name_hint is provided (e.g., "UserSchema"):
///    -> Prefer file whose name contains the hint prefix (e.g., "user.rs" for "UserSchema")
/// 3. Otherwise -> return None (ambiguous)
///
/// The `schema_name_hint` is the custom schema name (e.g., "UserSchema", "MemoSchema")
/// which often contains a hint about the module name.
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the inferred module path
/// from the file location (e.g., `["crate", "models", "user"]`).
pub fn find_struct_by_name_in_all_files(
    src_dir: &Path,
    struct_name: &str,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    // Collect all .rs files recursively
    let mut rs_files = Vec::new();
    collect_rs_files_recursive(src_dir, &mut rs_files);

    // Store: (file_path, struct_metadata)
    let mut found_structs: Vec<(std::path::PathBuf, StructMetadata)> = Vec::new();

    for file_path in rs_files {
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_ast = match syn::parse_file(&content) {
            Ok(ast) => ast,
            Err(_) => continue,
        };

        // Look for the struct in the file
        for item in &file_ast.items {
            if let syn::Item::Struct(struct_item) = item
                && struct_item.ident == struct_name
            {
                found_structs.push((
                    file_path.clone(),
                    StructMetadata::new_model(
                        struct_name.to_string(),
                        quote::quote!(#struct_item).to_string(),
                    ),
                ));
            }
        }
    }

    match found_structs.len() {
        0 => None,
        1 => {
            let (path, metadata) = found_structs.remove(0);
            let module_path = file_path_to_module_path(&path, src_dir);
            Some((metadata, module_path))
        }
        _ => {
            // Multiple structs with same name - try to disambiguate using schema_name_hint
            if let Some(hint) = schema_name_hint {
                // Extract prefix from schema name (e.g., "UserSchema" -> "user", "MemoSchema" -> "memo")
                let hint_lower = hint.to_lowercase();
                let prefix = hint_lower
                    .strip_suffix("schema")
                    .or_else(|| hint_lower.strip_suffix("response"))
                    .or_else(|| hint_lower.strip_suffix("request"))
                    .unwrap_or(&hint_lower);

                // Normalize prefix: remove underscores for comparison
                // This allows "AdminUserSchema" (prefix "adminuser") to match "admin_user.rs"
                let prefix_normalized = prefix.replace('_', "");

                // First, try exact filename match (normalized)
                // e.g., "admin_user.rs" normalized to "adminuser" matches prefix "adminuser"
                let exact_match: Vec<_> = found_structs
                    .iter()
                    .filter(|(path, _)| {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|name| {
                                name.to_lowercase().replace('_', "") == prefix_normalized
                            })
                    })
                    .collect();

                if exact_match.len() == 1 {
                    let (path, metadata) = exact_match[0];
                    let module_path = file_path_to_module_path(path, src_dir);
                    return Some((metadata.clone(), module_path));
                }

                // Fallback: Find files whose normalized name contains the prefix
                let matching: Vec<_> = found_structs
                    .into_iter()
                    .filter(|(path, _)| {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|name| {
                                name.to_lowercase()
                                    .replace('_', "")
                                    .contains(&prefix_normalized)
                            })
                    })
                    .collect();

                if matching.len() == 1 {
                    let (path, metadata) = matching.into_iter().next().unwrap();
                    let module_path = file_path_to_module_path(&path, src_dir);
                    return Some((metadata, module_path));
                }
            }

            // Still ambiguous
            None
        }
    }
}

/// Recursively collect all `.rs` files in a directory.
pub fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_recursive(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

/// Derive module path from a file path relative to src directory.
///
/// Examples:
/// - `src/models/user.rs` -> `["crate", "models", "user"]`
/// - `src/models/user/mod.rs` -> `["crate", "models", "user"]`
/// - `src/lib.rs` -> `["crate"]`
pub fn file_path_to_module_path(file_path: &Path, src_dir: &Path) -> Vec<String> {
    let relative = match file_path.strip_prefix(src_dir) {
        Ok(r) => r,
        Err(_) => return vec!["crate".to_string()],
    };

    let mut segments = vec!["crate".to_string()];

    for component in relative.components() {
        if let std::path::Component::Normal(os_str) = component
            && let Some(s) = os_str.to_str()
        {
            // Handle .rs extension
            if let Some(name) = s.strip_suffix(".rs") {
                // Skip mod.rs and lib.rs - they don't add a segment
                if name != "mod" && name != "lib" {
                    segments.push(name.to_string());
                }
            } else {
                // Directory name
                segments.push(s.to_string());
            }
        }
    }

    segments
}

/// Find struct definition from a schema path string (e.g., "crate::models::user::Schema").
///
/// Similar to `find_struct_from_path` but takes a string path instead of syn::Type.
pub fn find_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string into segments
    let segments: Vec<&str> = path_str.split("::").filter(|s| !s.is_empty()).collect();

    if segments.is_empty() {
        return None;
    }

    // The last segment is the struct name
    let struct_name = segments.last()?.to_string();

    // Build possible file paths from the module path
    // e.g., crate::models::user::Schema -> src/models/user.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some(StructMetadata::new_model(
                        struct_name.clone(),
                        quote::quote!(#struct_item).to_string(),
                    ));
                }
                _ => continue,
            }
        }
    }

    None
}

/// Find the Model definition from a Schema path.
/// Converts "crate::models::user::Schema" -> finds Model in src/models/user.rs
pub fn find_model_from_schema_path(schema_path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string and convert Schema path to module path
    // e.g., "crate :: models :: user :: Schema" -> ["crate", "models", "user"]
    let segments: Vec<&str> = schema_path_str
        .split("::")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "Schema")
        .collect();

    if segments.is_empty() {
        return None;
    }

    // Build possible file paths from the module path
    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for Model struct in the file
        for item in &file_ast.items {
            if let syn::Item::Struct(struct_item) = item
                && struct_item.ident == "Model"
            {
                return Some(StructMetadata::new_model(
                    "Model".to_string(),
                    quote::quote!(#struct_item).to_string(),
                ));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
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

        // Create some .rs files
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

    // ============================================================
    // Coverage tests for find_struct_from_path
    // ============================================================

    #[test]
    fn test_find_struct_from_path_non_path_type() {
        // Tests: Type is not a Path type -> returns None
        use syn::Type;

        // Create a reference type (not a path type)
        let ty: Type = syn::parse_str("&str").unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();

        // Set a temporary manifest dir (doesn't matter since we'll return early)
        let temp_dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result = find_struct_from_path(&ty, None);

        // Restore
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
    fn test_find_struct_from_path_empty_segments() {
        // Tests: Type path with empty segments -> returns None
        use syn::{Path, TypePath};

        // Construct a TypePath with empty segments
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
        // Tests: File contains items that are not the target struct
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create a file with multiple items, only one matching
        let content = r#"
pub enum SomeEnum { A, B }
pub fn some_function() {}
pub const SOME_CONST: i32 = 42;
pub trait SomeTrait {}
pub struct NotTarget { pub x: i32 }
pub struct Target { pub id: i32 }
"#;
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

    // ============================================================
    // Coverage tests for find_struct_by_name_in_all_files
    // ============================================================

    #[test]
    #[serial]
    fn test_find_struct_by_name_unreadable_file() {
        // Coverage for line 122: Err(_) => continue
        // Create broken symlink that exists but can't be read
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // Valid file
        std::fs::write(
            src_dir.join("valid.rs"),
            "pub struct Target { pub id: i32 }",
        )
        .unwrap();

        // Broken symlink -> read_to_string fails -> line 122
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
        // Tests: File cannot be parsed -> continue to next file
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // Create an unparseable file
        std::fs::write(src_dir.join("broken.rs"), "this is not valid rust {{{{").unwrap();

        // Create a valid file with the struct
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
        // Tests: Multiple structs with same name, schema_name_hint disambiguates
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // Create user.rs with Model
        std::fs::create_dir(src_dir.join("models")).unwrap();
        std::fs::write(
            src_dir.join("models").join("user.rs"),
            "pub struct Model { pub id: i32, pub name: String }",
        )
        .unwrap();

        // Create memo.rs with Model (same struct name)
        std::fs::write(
            src_dir.join("models").join("memo.rs"),
            "pub struct Model { pub id: i32, pub title: String }",
        )
        .unwrap();

        // Without hint - should return None (ambiguous)
        let result_no_hint = find_struct_by_name_in_all_files(src_dir, "Model", None);
        assert!(
            result_no_hint.is_none(),
            "Without hint, multiple Models should be ambiguous"
        );

        // With hint "UserSchema" - should find user.rs
        let result_with_hint =
            find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
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

        // With hint "MemoSchema" - should find memo.rs
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

        // With hint "UserResponse" - should find user.rs
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

        // With hint "UserRequest" - should find user.rs
        let result = find_struct_by_name_in_all_files(src_dir, "Input", Some("UserRequest"));
        assert!(
            result.is_some(),
            "With UserRequest hint, should find user.rs"
        );
    }

    #[test]
    #[serial]
    fn test_find_struct_disambiguation_still_ambiguous() {
        // Tests: Multiple matches even after applying hint -> returns None
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // Create two files that both match the hint
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

        // With hint "UserSchema" - both user_admin.rs and user_regular.rs match
        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
        assert!(
            result.is_none(),
            "Multiple files matching hint should still be ambiguous"
        );
    }

    #[test]
    #[serial]
    fn test_find_struct_disambiguation_snake_case_filename() {
        // Tests: CamelCase schema name matches snake_case filename
        // e.g., "AdminUserSchema" should match "admin_user.rs"
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::create_dir(src_dir.join("models")).unwrap();
        // Create admin_user.rs with Model
        std::fs::write(
            src_dir.join("models").join("admin_user.rs"),
            "pub struct Model { pub id: i32, pub role: String }",
        )
        .unwrap();
        // Create regular_user.rs with Model
        std::fs::write(
            src_dir.join("models").join("regular_user.rs"),
            "pub struct Model { pub id: i32, pub name: String }",
        )
        .unwrap();

        // With hint "AdminUserSchema" - should find admin_user.rs
        // "AdminUserSchema" -> prefix "adminuser" -> matches "admin_user.rs" (normalized: "adminuser")
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

        // With hint "RegularUserSchema" - should find regular_user.rs
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

    // ============================================================
    // Coverage tests for find_struct_from_schema_path
    // ============================================================

    #[test]
    fn test_find_struct_from_schema_path_empty_string() {
        // Tests: Empty path string -> returns None
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
    fn test_find_struct_from_schema_path_no_module() {
        // Tests: Path with only struct name (no module) -> returns None
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        let temp_dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Only "Schema" with no module path - after filtering crate/self/super, module_segments is empty
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
        // Tests: File contains non-struct items that get skipped
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        let content = r#"
pub enum NotStruct { A, B }
pub fn not_struct() {}
pub struct Target { pub id: i32 }
pub const NOT_STRUCT: i32 = 1;
"#;
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

    // ============================================================
    // Coverage tests for find_model_from_schema_path
    // ============================================================

    #[test]
    fn test_find_model_from_schema_path_empty_after_filter() {
        // Tests: After filtering "Schema" and other keywords, segments is empty
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        let temp_dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Only "Schema" - after filtering, empty
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
    fn test_find_model_from_schema_path_no_module() {
        // Tests: After filtering crate/self/super, module_segments is empty
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        let temp_dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // "crate::Schema" - after filtering "Schema" and "crate", module_segments is empty
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
        // Tests: No exact match, but fallback "contains" finds exactly one match
        // This covers lines 169-174 (the fallback contains path)
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::create_dir(src_dir.join("models")).unwrap();
        // No file named exactly "special.rs", but "special_item.rs" contains "special"
        std::fs::write(
            src_dir.join("models").join("special_item.rs"),
            "pub struct Model { pub special_field: i32 }",
        )
        .unwrap();
        // Another file that doesn't match
        std::fs::write(
            src_dir.join("models").join("regular.rs"),
            "pub struct Model { pub regular_field: String }",
        )
        .unwrap();

        // With hint "SpecialSchema" -> prefix "special"
        // No exact match (no "special.rs"), but "special_item.rs" contains "special"
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
}
