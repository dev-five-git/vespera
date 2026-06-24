//! Struct lookup/search helpers.

use std::path::{Path, PathBuf};

use syn::Type;

use crate::metadata::StructMetadata;

/// Why a source struct lookup failed.
#[derive(Debug, Clone)]
pub enum LookupError {
    /// The macro could not derive a usable path from the supplied type.
    InvalidTypePath,
    /// `CARGO_MANIFEST_DIR` was unavailable.
    MissingManifestDir,
    /// No matching struct definition was found.
    NotFound {
        struct_name: String,
        searched: Vec<PathBuf>,
    },
    /// A bare source name was not found in the macro call-site file.
    BareNotFound { struct_name: String },
}

impl LookupError {
    /// Convert a lookup failure into a user-facing macro diagnostic.
    pub fn to_syn_error(&self, span: &impl quote::ToTokens) -> syn::Error {
        match self {
            Self::InvalidTypePath => syn::Error::new_spanned(
                span,
                "schema_type! source must be a type path like `Model` or `crate::models::user::Model`",
            ),
            Self::MissingManifestDir => syn::Error::new_spanned(
                span,
                "schema_type! source type not found: CARGO_MANIFEST_DIR is not set",
            ),
            Self::NotFound {
                struct_name,
                searched,
            } => syn::Error::new_spanned(
                span,
                format!(
                    "schema_type! struct `{struct_name}` not found. Searched: {}",
                    render_paths(searched)
                ),
            ),
            Self::BareNotFound { struct_name } => syn::Error::new_spanned(
                span,
                format!(
                    "struct `{struct_name}` not found in this file; use a qualified path like `crate::models::<module>::{struct_name}`"
                ),
            ),
        }
    }
}

fn render_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        "<none>".to_string()
    } else {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

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
/// For simple names (e.g., just `Model` without module path), it only checks the
/// macro call-site file. This supports same-file usage like:
/// ```ignore
/// pub struct Model { ... }
/// vespera::schema_type!(Schema from Model, name = "UserSchema");
/// ```
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the module path.
/// For qualified paths, this is extracted from the type itself.
/// For simple names, it's inferred from the file location.
#[cfg(test)]
pub fn find_struct_from_path(
    ty: &Type,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    find_struct_from_path_detailed(ty, schema_name_hint).ok()
}

/// Detailed variant of [`find_struct_from_path`] that preserves failure reasons.
pub fn find_struct_from_path_detailed(
    ty: &Type,
    _schema_name_hint: Option<&str>,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()
        .ok_or(LookupError::MissingManifestDir)?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Extract path segments from the type
    let Type::Path(type_path) = ty else {
        return Err(LookupError::InvalidTypePath);
    };

    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    if segments.is_empty() {
        return Err(LookupError::InvalidTypePath);
    }

    // The last segment is the struct name
    let struct_name = segments.last().ok_or(LookupError::InvalidTypePath)?.clone();

    // Build possible file paths from the module path
    // e.g., models::memo::Model -> src/models/memo.rs or src/models/memo/mod.rs
    // e.g., crate::models::memo::Model -> src/models/memo.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
        .map(std::string::String::as_str)
        .collect();

    // If no module path (simple name like `Model`), resolve only in the macro
    // call-site file. Cross-file bare lookup is action-at-a-distance: callers
    // must use a qualified path for anything outside the file that invokes the
    // macro.
    if module_segments.is_empty() {
        return find_bare_struct_in_call_site(&src_dir, &struct_name);
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
            return Ok((
                StructMetadata::new_model(struct_name, definition),
                type_module_path,
            ));
        }
    }

    Err(LookupError::NotFound {
        struct_name,
        searched: candidate_file_paths(&src_dir, &module_segments)
            .into_iter()
            .collect(),
    })
}

fn find_bare_struct_in_call_site(
    src_dir: &Path,
    struct_name: &str,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    let Some(file_path) = proc_macro2::Span::call_site().local_file() else {
        return Err(LookupError::BareNotFound {
            struct_name: struct_name.to_string(),
        });
    };
    let Some(definition) =
        crate::schema_macro::file_cache::get_struct_definition(&file_path, struct_name)
    else {
        return Err(LookupError::BareNotFound {
            struct_name: struct_name.to_string(),
        });
    };
    Ok((
        StructMetadata::new_model(struct_name.to_string(), definition),
        file_path_to_module_path(&file_path, src_dir),
    ))
}

/// Recursively collect all `.rs` files in a directory.
pub fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        // `entry.file_type()` reads the kind from the directory-entry data the
        // OS already returned for this `read_dir` walk — no extra `metadata`
        // stat per entry, unlike `path.is_dir()`.  Mirrors the established
        // `file_utils::collect_with_mtimes_into` pattern (symlinks, which are
        // neither file nor dir here, are skipped — never present in a `src/`
        // tree this indexes).
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_rs_files_recursive(&path, files);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
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
    let relative = file_path
        .strip_prefix(src_dir)
        .ok()
        .map(std::path::Path::to_path_buf)
        .or_else(|| relative_path_after_src(file_path))
        .unwrap_or_default();

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

fn relative_path_after_src(file_path: &Path) -> Option<PathBuf> {
    let mut seen_src = false;
    let mut relative = PathBuf::new();
    for component in file_path.components() {
        let std::path::Component::Normal(os_str) = component else {
            continue;
        };
        if seen_src {
            relative.push(os_str);
        } else if os_str == "src" {
            seen_src = true;
        }
    }
    seen_src.then_some(relative)
}

/// Find struct definition from a schema path string (e.g., "`crate::models::user::Schema`").
///
/// Similar to `find_struct_from_path` but takes a string path instead of `syn::Type`.
pub fn find_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string into segments
    let segments: Vec<&str> = path_str
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

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
mod tests;
