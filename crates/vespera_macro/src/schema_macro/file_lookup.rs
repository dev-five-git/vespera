//! File lookup utilities for schema macros
//!
//! Contains functions for finding struct definitions across source files:
//! - Finding structs from module paths
//! - Scanning source directories for struct definitions
//! - Converting file paths to module paths

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
    // e.g., crate::models::memo::Model → ["crate", "models", "memo"]
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
/// 1. If exactly one struct with the name exists → use it
/// 2. If multiple exist and schema_name_hint is provided (e.g., "UserSchema"):
///    → Prefer file whose name contains the hint prefix (e.g., "user.rs" for "UserSchema")
/// 3. Otherwise → return None (ambiguous)
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

                // Find files whose name contains the prefix
                let matching: Vec<_> = found_structs
                    .into_iter()
                    .filter(|(path, _)| {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|name| name.to_lowercase().contains(prefix))
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
/// - `src/models/user.rs` → `["crate", "models", "user"]`
/// - `src/models/user/mod.rs` → `["crate", "models", "user"]`
/// - `src/lib.rs` → `["crate"]`
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
