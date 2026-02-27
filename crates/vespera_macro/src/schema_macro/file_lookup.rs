//! File system operations for finding struct definitions
//!
//! Provides functions to locate struct definitions in source files.

use std::path::Path;

use syn::Type;

use crate::metadata::StructMetadata;
use std::path::PathBuf;

/// Build candidate file paths from module segments.
///
/// Given a source directory and module segments (e.g., `["models", "memo"]`),
/// returns both `{src_dir}/models/memo.rs` and `{src_dir}/models/memo/mod.rs`.
#[inline]
fn candidate_file_paths(src_dir: &Path, module_segments: &[&str]) -> [PathBuf; 2] {
    let joined = module_segments.join("/");
    [
        src_dir.join(format!("{joined}.rs")),
        src_dir.join(format!("{joined}/mod.rs")),
    ]
}

/// Try to find a struct definition from a module path by reading source files.
///
/// This allows `schema_type`! to work with structs defined in other files, like:
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
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = super::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Extract path segments from the type
    let Type::Path(type_path) = ty else {
        return None;
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
        .map(std::string::String::as_str)
        .collect();

    // If no module path (simple name like `Model`), scan all files with schema_name hint
    if module_segments.is_empty() {
        return find_struct_by_name_in_all_files(&src_dir, &struct_name, schema_name_hint);
    }

    // For qualified paths, the module path is extracted from the type itself
    // e.g., crate::models::memo::Model -> ["crate", "models", "memo"]
    let type_module_path: Vec<String> = segments[..segments.len() - 1].to_vec();

    // Try different file path patterns
    let file_paths = candidate_file_paths(&src_dir, &module_segments);

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let file_ast = super::file_cache::get_parsed_ast(&file_path)?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some((
                        StructMetadata::new_model(
                            struct_name,
                            quote::quote!(#struct_item).to_string(),
                        ),
                        type_module_path,
                    ));
                }
                _ => {}
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
/// 2. If multiple exist and `schema_name_hint` is provided (e.g., "UserSchema"):
///    -> Prefer file whose name contains the hint prefix (e.g., "user.rs" for "`UserSchema`")
/// 3. Otherwise -> return None (ambiguous)
///
/// The `schema_name_hint` is the custom schema name (e.g., "`UserSchema`", "`MemoSchema`")
/// which often contains a hint about the module name.
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the inferred module path
/// from the file location (e.g., `["crate", "models", "user"]`).
#[allow(clippy::too_many_lines)]
pub fn find_struct_by_name_in_all_files(
    src_dir: &Path,
    struct_name: &str,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    // Use cached struct-candidate index: files already filtered by text search
    let mut rs_files = super::file_cache::get_struct_candidates(src_dir, struct_name);

    // Pre-compute hint prefix once (used in fast path and fallback disambiguation)
    let prefix_normalized = schema_name_hint.map(derive_hint_prefix);

    // FAST PATH: If schema_name_hint is provided, try matching files first.
    // This avoids parsing ALL files for the common same-file pattern:
    //   schema_type!(Schema from Model, name = "UserSchema")  in user.rs
    if let Some(prefix_normalized) = &prefix_normalized {
        // Partition files: candidate files (filename matches hint prefix) vs rest
        let (candidates, rest): (Vec<_>, Vec<_>) = rs_files.into_iter().partition(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|name| {
                    let norm = normalize_name(name);
                    norm == *prefix_normalized || norm.contains(prefix_normalized.as_str())
                })
        });

        // Parse only candidate files first
        let mut found_in_candidates: Vec<(std::path::PathBuf, StructMetadata)> = Vec::new();
        for file_path in &candidates {
            let Some(file_ast) = super::file_cache::get_parsed_ast(file_path) else {
                continue;
            };
            for item in &file_ast.items {
                if let syn::Item::Struct(struct_item) = item
                    && struct_item.ident == struct_name
                {
                    found_in_candidates.push((
                        file_path.clone(),
                        StructMetadata::new_model(
                            struct_name.to_string(),
                            quote::quote!(#struct_item).to_string(),
                        ),
                    ));
                }
            }
        }

        // If exactly one match in candidates, return immediately (fast path hit!)
        if found_in_candidates.len() == 1 {
            let (path, metadata) = found_in_candidates.remove(0);
            let module_path = file_path_to_module_path(&path, src_dir);
            return Some((metadata, module_path));
        }

        // If candidates found multiple, try disambiguation by exact filename match
        if found_in_candidates.len() > 1 {
            let exact_match: Vec<_> = found_in_candidates
                .iter()
                .filter(|(path, _)| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|name| normalize_name(name) == *prefix_normalized)
                })
                .collect();

            if exact_match.len() == 1 {
                let (path, metadata) = exact_match[0];
                let module_path = file_path_to_module_path(path, src_dir);
                return Some((metadata.clone(), module_path));
            }

            // Still ambiguous among candidates
            return None;
        }

        // No match in candidates — fall through to scan remaining files
        rs_files = rest;
    }

    // FULL SCAN: Parse all remaining files (or all files if no hint)
    let mut found_structs: Vec<(std::path::PathBuf, StructMetadata)> = Vec::new();

    for file_path in rs_files {
        let Some(file_ast) = super::file_cache::get_parsed_ast(&file_path) else {
            continue;
        };

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
        1 => {
            let (path, metadata) = found_structs.remove(0);
            let module_path = file_path_to_module_path(&path, src_dir);
            Some((metadata, module_path))
        }
        _ => None,
    }
}

/// Derive a normalized prefix from a schema name hint for file matching.
///
/// Strips common suffixes ("Schema", "Response", "Request") and normalizes
/// by removing underscores and lowercasing.
///
/// # Examples
/// - "UserSchema" → "user"
/// - "MemoResponse" → "memo"
/// - "AdminUserSchema" → "adminuser"
fn derive_hint_prefix(hint: &str) -> String {
    let hint_lower = hint.to_lowercase();
    let prefix = hint_lower
        .strip_suffix("schema")
        .or_else(|| hint_lower.strip_suffix("response"))
        .or_else(|| hint_lower.strip_suffix("request"))
        .unwrap_or(&hint_lower);
    normalize_name(prefix)
}

/// Normalize a name by lowercasing and removing underscores in a single pass.
/// Replaces the two-allocation `s.to_lowercase().replace('_', "")` pattern.
#[inline]
fn normalize_name(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Recursively collect all `.rs` files in a directory.
pub fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
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
    let Ok(relative) = file_path.strip_prefix(src_dir) else {
        return vec!["crate".to_string()];
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

/// Find struct definition from a schema path string (e.g., "`crate::models::user::Schema`").
///
/// Similar to `find_struct_from_path` but takes a string path instead of `syn::Type`.
pub fn find_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = super::file_cache::get_manifest_dir()?;
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
    let file_paths = candidate_file_paths(&src_dir, &module_segments);

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let file_ast = super::file_cache::get_parsed_ast(&file_path)?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some(StructMetadata::new_model(
                        struct_name,
                        quote::quote!(#struct_item).to_string(),
                    ));
                }
                _ => {}
            }
        }
    }

    None
}

/// Find the FK column name from the target entity for a `HasMany` relation with `via_rel`.
///
/// When a `HasMany` relation has `via_rel = "TargetUser"`, this function:
/// 1. Looks up the target entity file (e.g., notification.rs from schema path)
/// 2. Finds the field with matching `relation_enum = "TargetUser"`
/// 3. Extracts and returns the `from` attribute value (e.g., "`target_user_id`")
///
/// Returns None if the target file can't be found or parsed, or if no matching relation exists.
#[allow(clippy::too_many_lines)]
pub fn find_fk_column_from_target_entity(
    target_schema_path: &str,
    via_rel: &str,
) -> Option<String> {
    use crate::schema_macro::seaorm::{extract_belongs_to_from_field, extract_relation_enum};

    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = super::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the schema path to get file path
    // e.g., "crate :: models :: notification :: Schema" -> src/models/notification.rs
    let segments: Vec<&str> = target_schema_path
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "Schema" && *s != "Entity")
        .collect();

    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = candidate_file_paths(&src_dir, &module_segments);

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let file_ast = super::file_cache::get_parsed_ast(&file_path)?;

        // Look for Model struct in the file
        for item in &file_ast.items {
            if let syn::Item::Struct(struct_item) = item
                && struct_item.ident == "Model"
            {
                // Search through fields for the one with matching relation_enum
                if let syn::Fields::Named(fields_named) = &struct_item.fields {
                    for field in &fields_named.named {
                        let field_relation_enum = extract_relation_enum(&field.attrs);
                        if field_relation_enum.as_deref() == Some(via_rel) {
                            // Found the matching field, extract FK column from `from` attribute
                            return extract_belongs_to_from_field(&field.attrs);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Find the Model definition from a Schema path.
/// Converts "`crate::models::user::Schema`" -> finds Model in src/models/user.rs
#[allow(clippy::too_many_lines)]
pub fn find_model_from_schema_path(schema_path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = super::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string and convert Schema path to module path
    // e.g., "crate :: models :: user :: Schema" -> ["crate", "models", "user"]
    let segments: Vec<&str> = schema_path_str
        .split("::")
        .map(str::trim)
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
    let file_paths = candidate_file_paths(&src_dir, &module_segments);

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let file_ast = super::file_cache::get_parsed_ast(&file_path)?;

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
    use std::path::Path;

    use serial_test::serial;
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

    // ============================================================
    // Coverage tests for find_struct_by_name_in_all_files
    // ============================================================

    #[test]
    #[serial]
    fn test_find_struct_by_name_unreadable_file() {
        // Tests for error continuation
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
        // Tests for fallback contains path
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

    // ============================================================
    // Tests for find_fk_column_from_target_entity
    // ============================================================

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_success() {
        // Tests: Full success path - find FK column from target entity
        // Full success path
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create notification.rs with a BelongsTo relation that has relation_enum matching via_rel
        let notification_model = r#"
pub struct Model {
    pub id: i32,
    pub message: String,
    pub target_user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "target_user_id", to = "id", relation_enum = "TargetUser")]
    pub target_user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("notification.rs"), notification_model).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "TargetUser");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert_eq!(
            result,
            Some("target_user_id".to_string()),
            "Should find FK column 'target_user_id'"
        );
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_mod_rs() {
        // Tests: Find FK column from mod.rs file
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models").join("notification");
        std::fs::create_dir_all(&models_dir).unwrap();

        let notification_model = r#"
pub struct Model {
    pub id: i32,
    pub sender_id: i32,
    #[sea_orm(belongs_to = "super::super::user::Entity", from = "sender_id", to = "id", relation_enum = "Sender")]
    pub sender: BelongsTo<super::super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("mod.rs"), notification_model).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "Sender");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert_eq!(
            result,
            Some("sender_id".to_string()),
            "Should find FK column from mod.rs"
        );
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_empty_module_segments() {
        // Tests: Empty module segments return None
        let temp_dir = TempDir::new().unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // After filtering "crate", "Schema", segments is empty
        let result = find_fk_column_from_target_entity("crate::Schema", "SomeRelation");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_none(), "Empty module segments should return None");
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_file_not_found() {
        // Tests: File does not exist -> continue, then return None
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Path to non-existent file
        let result =
            find_fk_column_from_target_entity("crate::models::nonexistent::Schema", "SomeRelation");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_none(), "Non-existent file should return None");
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_unparseable_file() {
        // Tests: File cannot be parsed -> returns None
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create unparseable file
        std::fs::write(models_dir.join("broken.rs"), "this is not valid rust {{{{").unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::broken::Schema", "SomeRelation");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_none(), "Unparseable file should return None");
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_no_model_struct() {
        // Tests: File exists but has no Model struct
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create file without Model struct
        let content = r"
pub struct SomethingElse {
    pub id: i32,
}
pub enum Status { Active, Inactive }
";
        std::fs::write(models_dir.join("nomodel.rs"), content).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::nomodel::Schema", "SomeRelation");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(
            result.is_none(),
            "File without Model struct should return None"
        );
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_no_matching_relation_enum() {
        // Tests: Model exists but no field matches the via_rel
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create model with different relation_enum
        let model = r#"
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id", relation_enum = "Author")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("comment.rs"), model).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Search for "TargetUser" but only "Author" exists
        let result =
            find_fk_column_from_target_entity("crate::models::comment::Schema", "TargetUser");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(
            result.is_none(),
            "Non-matching relation_enum should return None"
        );
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_tuple_struct() {
        // Tests: Model is a tuple struct (not named fields) -> skip
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create tuple struct Model
        let model = "pub struct Model(i32, String);";
        std::fs::write(models_dir.join("tuple.rs"), model).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::tuple::Schema", "SomeRelation");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_none(), "Tuple struct Model should return None");
    }

    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_field_no_from_attr() {
        // Tests: Field matches relation_enum but has no `from` attribute
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create model with relation_enum but no `from` attribute
        let model = r#"
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", to = "id", relation_enum = "TargetUser")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("nofrom.rs"), model).unwrap();

        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result =
            find_fk_column_from_target_entity("crate::models::nofrom::Schema", "TargetUser");

        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        // extract_belongs_to_from_field returns None when no `from` attr
        assert!(
            result.is_none(),
            "Field without 'from' attribute should return None"
        );
    }

    // ============================================================
    // Coverage tests for find_struct_by_name_in_all_files (candidate/rest paths)
    // ============================================================

    #[test]
    #[serial]
    fn test_find_struct_candidate_unparseable_file() {
        // Tests line 145: candidate file fails to parse -> continue to next candidate
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // user.rs matches hint prefix "user" (candidate), contains "Model" text, but won't parse
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Model {{{{ broken syntax",
        )
        .unwrap();

        // valid.rs contains Model and parses fine (goes to rest since filename doesn't match prefix)
        std::fs::write(src_dir.join("valid.rs"), "pub struct Model { pub id: i32 }").unwrap();

        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));

        assert!(
            result.is_some(),
            "Should find Model in valid.rs after skipping unparseable candidate user.rs"
        );
    }

    #[test]
    #[serial]
    fn test_find_struct_exact_filename_disambiguation() {
        // Tests lines 168-170: multiple candidates found, exact filename match disambiguates
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // user.rs: exact match (normalize_name("user") == prefix "user")
        std::fs::write(src_dir.join("user.rs"), "pub struct Model { pub id: i32 }").unwrap();

        // user_extended.rs: contains-match only (normalize_name("user_extended") = "userextended" != "user")
        std::fs::write(
            src_dir.join("user_extended.rs"),
            "pub struct Model { pub name: String }",
        )
        .unwrap();

        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));

        assert!(result.is_some(), "Should resolve via exact filename match");
        let (metadata, _) = result.unwrap();
        assert!(
            metadata.definition.contains("id"),
            "Should return user.rs Model (with id field)"
        );
    }

    #[test]
    #[serial]
    fn test_find_struct_no_match_in_candidates_falls_to_rest() {
        // Tests line 189: candidates have no struct match -> rs_files = rest -> full scan finds it
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // user.rs is a candidate (filename matches "user" prefix) but has no struct Model
        // Must contain "Model" text for get_struct_candidates to include it
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Other { pub x: i32 } // Model ref",
        )
        .unwrap();

        // data.rs is in rest (filename "data" doesn't contain "user"), has struct Model
        std::fs::write(src_dir.join("data.rs"), "pub struct Model { pub id: i32 }").unwrap();

        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));

        assert!(
            result.is_some(),
            "Should find Model in data.rs after candidates had no match"
        );
    }

    #[test]
    #[serial]
    fn test_find_struct_full_scan_unparseable_file() {
        // Tests line 197: full-scan file fails to parse -> continue to next file
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        // user.rs is candidate but no struct Model
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Other { pub x: i32 } // Model",
        )
        .unwrap();

        // broken.rs is rest, contains "Model" text but won't parse
        std::fs::write(src_dir.join("broken.rs"), "Model unparseable {{{{{").unwrap();

        // valid.rs is rest, has struct Model
        std::fs::write(src_dir.join("valid.rs"), "pub struct Model { pub id: i32 }").unwrap();

        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));

        assert!(
            result.is_some(),
            "Should find Model in valid.rs after skipping unparseable broken.rs in rest"
        );
    }
}
