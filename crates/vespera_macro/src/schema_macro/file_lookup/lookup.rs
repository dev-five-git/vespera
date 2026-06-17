//! Struct lookup/search helpers.

use std::path::{Path, PathBuf};

use syn::Type;

use crate::metadata::StructMetadata;

/// Build candidate file paths from module segments.
///
/// Given a source directory and module segments (e.g., `["models", "memo"]`),
/// returns both `{src_dir}/models/memo.rs` and `{src_dir}/models/memo/mod.rs`.
#[inline]
pub(super) fn candidate_file_paths(src_dir: &Path, module_segments: &[&str]) -> [PathBuf; 2] {
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
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
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
        // No `exists()` preflight: `get_struct_definition` reads through the
        // mtime-validated cache and returns `None` for a missing/unreadable
        // file, so the extra stat (and its TOCTOU window) is pure overhead.
        if let Some(definition) =
            crate::schema_macro::file_cache::get_struct_definition(&file_path, &struct_name)
        {
            return Some((
                StructMetadata::new_model(struct_name, definition),
                type_module_path,
            ));
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
    // Use cached struct-candidate index: files already filtered by text
    // search.  `Arc<[PathBuf]>` — iterate by reference; only matched
    // paths are cloned.
    let all_files = crate::schema_macro::file_cache::get_struct_candidates(src_dir, struct_name);
    let mut rs_files: Vec<&std::path::PathBuf> = all_files.iter().collect();

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
            if let Some(definition) =
                crate::schema_macro::file_cache::get_struct_definition(file_path, struct_name)
            {
                found_in_candidates.push((
                    (*file_path).clone(),
                    StructMetadata::new_model(struct_name.to_string(), definition),
                ));
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
        if let Some(definition) =
            crate::schema_macro::file_cache::get_struct_definition(file_path, struct_name)
        {
            found_structs.push((
                file_path.clone(),
                StructMetadata::new_model(struct_name.to_string(), definition),
            ));
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
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
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
        // No `exists()` preflight: the mtime-validated cache read returns
        // `None` for a missing/unreadable file, so the stat is redundant
        // (and TOCTOU-prone).
        if let Some(definition) =
            crate::schema_macro::file_cache::get_struct_definition(&file_path, &struct_name)
        {
            return Some(StructMetadata::new_model(struct_name, definition));
        }
    }

    None
}

/// Find the Model definition from a Schema path.
/// Converts "`crate::models::user::Schema`" -> finds Model in src/models/user.rs
#[allow(clippy::too_many_lines)]
pub fn find_model_from_schema_path(schema_path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
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
        // No `exists()` preflight: the mtime-validated cache read returns
        // `None` for a missing/unreadable file, so the stat is redundant
        // (and TOCTOU-prone).
        if let Some(definition) =
            crate::schema_macro::file_cache::get_struct_definition(&file_path, "Model")
        {
            return Some(StructMetadata::new_model("Model".to_string(), definition));
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
}
