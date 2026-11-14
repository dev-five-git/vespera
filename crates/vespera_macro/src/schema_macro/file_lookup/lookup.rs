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
/// 1. Parse the path (e.g., `models::memo::Model`, `crate::models::memo::Model`,
///    `crate::Model`, `self::inner::Model`, `super::memo::Model`)
/// 2. Look the struct up in the module's own file (`src/models/memo.rs` or
///    `src/models/memo/mod.rs`), then by walking the module tree from the
///    crate root (`lib.rs` / `main.rs`, `#[path]` modules, inline `mod`s)
///
/// For simple names (e.g., just `Model` without module path), it only checks the
/// macro call-site file (inline modules included). This supports same-file usage like:
/// ```ignore
/// pub struct Model { ... }
/// vespera::schema_type!(Schema from Model, name = "UserSchema");
/// ```
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the absolute
/// module path (`["crate", "models", "memo"]`).
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
    let call_site = proc_macro2::Span::call_site().local_file();
    find_struct_with_call_site(ty, call_site.as_deref())
}

/// [`find_struct_from_path_detailed`] with the macro call-site file passed
/// explicitly (unit tests have none).
pub(super) fn find_struct_with_call_site(
    ty: &Type,
    call_site: Option<&Path>,
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
        .map(|s| crate::parser::strip_raw_prefix_owned(s.ident.to_string()))
        .collect();

    // The last segment is the struct name
    let (struct_name, module_part) = segments.split_last().ok_or(LookupError::InvalidTypePath)?;
    let struct_name = struct_name.clone();

    // A bare name (`Model`) resolves only in the macro call-site file.
    // Cross-file bare lookup is action-at-a-distance: callers must use a
    // qualified path for anything outside the file that invokes the macro.
    if module_part.is_empty() {
        return match call_site {
            Some(file_path) => find_bare_struct_in_file(file_path, &struct_name, &src_dir),
            None => Err(LookupError::BareNotFound { struct_name }),
        };
    }

    let call_site_module = call_site.map(|file| file_path_to_module_path(file, &src_dir));
    let module_path = absolute_module_path(module_part, call_site_module.as_deref());
    let module_segments: Vec<&str> = module_path[1..].iter().map(String::as_str).collect();

    if let Some(definition) = find_struct_in_module(&src_dir, &module_segments, &struct_name) {
        return Ok((
            StructMetadata::new_model(struct_name, definition),
            module_path,
        ));
    }

    let mut searched: Vec<PathBuf> = if module_segments.is_empty() {
        Vec::new()
    } else {
        candidate_file_paths(&src_dir, &module_segments).into()
    };
    searched.extend(crate::schema_macro::module_scope::crate_root_files(
        &src_dir,
    ));
    Err(LookupError::NotFound {
        struct_name,
        searched,
    })
}

/// The absolute (`crate::..`) module path of a written module path:
/// `crate::a` as is, `self::a` / `super::a` relative to the call-site
/// module (when known), anything else relative to the crate root.
fn absolute_module_path(
    module_part: &[String],
    call_site_module: Option<&[String]>,
) -> Vec<String> {
    let first = module_part[0].as_str();
    match (first, call_site_module) {
        ("crate", _) => module_part.to_vec(),
        ("self", Some(module)) => module.iter().chain(&module_part[1..]).cloned().collect(),
        ("super", Some(module)) => {
            let supers = module_part.iter().take_while(|s| *s == "super").count();
            let keep = module.len().saturating_sub(supers).max(1);
            module[..keep]
                .iter()
                .chain(&module_part[supers..])
                .cloned()
                .collect()
        }
        _ => std::iter::once("crate".to_string())
            .chain(
                module_part
                    .iter()
                    .filter(|s| *s != "self" && *s != "super")
                    .cloned(),
            )
            .collect(),
    }
}

/// Definition of `struct_name` in the crate module `module_segments`
/// (crate-relative; empty = crate root): the module's own file first (fast,
/// also for trees without a crate root on disk), then the module tree.
pub(super) fn find_struct_in_module(
    src_dir: &Path,
    module_segments: &[&str],
    struct_name: &str,
) -> Option<String> {
    if !module_segments.is_empty() {
        for file_path in candidate_file_paths(src_dir, module_segments) {
            // No `exists()` preflight: `get_struct_definition` reads through the
            // mtime-validated cache and returns `None` for a missing/unreadable
            // file, so the extra stat (and its TOCTOU window) is pure overhead.
            if let Some(definition) =
                crate::schema_macro::file_cache::get_struct_definition(&file_path, struct_name)
            {
                return Some(definition);
            }
        }
    }
    let module_path: Vec<String> = module_segments.iter().map(ToString::to_string).collect();
    let items = crate::schema_macro::module_scope::module_items(&module_path)?;
    crate::schema_macro::module_scope::struct_in_items(&items, struct_name)
}

/// Resolves a bare struct in the supplied file — at its top level or, when
/// exactly one candidate exists, inside an inline `mod` — preserving
/// call-site-only lookup while taking the resolved path explicitly so
/// filesystem behavior is testable.
fn find_bare_struct_in_file(
    file_path: &Path,
    struct_name: &str,
    src_dir: &Path,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    let module_path = file_path_to_module_path(file_path, src_dir);
    if let Some(definition) =
        crate::schema_macro::file_cache::get_struct_definition(file_path, struct_name)
    {
        return Ok((
            StructMetadata::new_model(struct_name.to_string(), definition),
            module_path,
        ));
    }
    let nested = crate::schema_macro::file_cache::get_parsed_file(file_path).and_then(|file| {
        crate::schema_macro::module_scope::unique_nested_struct(&file.items, struct_name)
    });
    let Some((definition, chain)) = nested else {
        return Err(LookupError::BareNotFound {
            struct_name: struct_name.to_string(),
        });
    };
    Ok((
        StructMetadata::new_model(struct_name.to_string(), definition),
        module_path.into_iter().chain(chain).collect(),
    ))
}

/// Classifies one already-typed directory entry, keeping the recursive collector's
/// file/dir invariant while accepting scan results as explicit inputs.
fn collect_rs_entry(
    entry: &std::fs::DirEntry,
    file_type: std::fs::FileType,
    files: &mut Vec<PathBuf>,
) {
    let path = entry.path();
    if file_type.is_dir() {
        collect_rs_files_recursive(&path, files);
    } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
        files.push(path);
    }
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
        if let Ok(file_type) = entry.file_type() {
            collect_rs_entry(&entry, file_type, files);
        }
    }
}

/// Derive module path from a file path relative to src directory.
///
/// Examples:
/// - `src/models/user.rs` -> `["crate", "models", "user"]`
/// - `src/models/user/mod.rs` -> `["crate", "models", "user"]`
/// - `src/lib.rs` / `src/main.rs` / `src/bin/tool.rs` -> `["crate"]`
pub fn file_path_to_module_path(file_path: &Path, src_dir: &Path) -> Vec<String> {
    let relative = file_path
        .strip_prefix(src_dir)
        .ok()
        .map(std::path::Path::to_path_buf)
        .or_else(|| relative_path_after_src(file_path))
        .unwrap_or_default();

    let mut names: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(os_str) => os_str.to_str().map(str::to_string),
            _ => None,
        })
        .collect();
    // Crate roots: `main.rs` / `lib.rs` at the top, and every binary under
    // `src/bin/` (`bin/tool.rs`, `bin/tool/main.rs`).
    let is_root = match names.as_slice() {
        [file] => file == "main.rs" || file == "lib.rs",
        [bin, file] => bin == "bin" && Path::new(file).extension().is_some_and(|ext| ext == "rs"),
        [bin, _, file] => bin == "bin" && file == "main.rs",
        _ => false,
    };
    if is_root {
        names.clear();
    }

    let mut segments = vec!["crate".to_string()];
    for name in names {
        // Handle .rs extension
        if let Some(stem) = name.strip_suffix(".rs") {
            // `mod.rs` does not add a segment
            if stem != "mod" {
                segments.push(stem.to_string());
            }
        } else {
            // Directory name
            segments.push(name);
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
/// `self` / `super` segments are dropped (relation paths reach here already
/// absolutized); the module is looked up through its own file, then the
/// module tree (crate root, inline / `#[path]` modules).
pub fn find_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    let segments = schema_path_segments(path_str, false);
    let (struct_name, module_segments) = segments.split_last()?;
    find_in_schema_module(module_segments, struct_name)
}

/// Find the Model definition from a Schema path.
/// Converts "`crate::models::user::Schema`" -> finds `Model` in the
/// `crate::models::user` module.
pub fn find_model_from_schema_path(schema_path_str: &str) -> Option<StructMetadata> {
    let module_segments = schema_path_segments(schema_path_str, true);
    if module_segments.is_empty() {
        return None;
    }
    find_in_schema_module(&module_segments, "Model")
}

/// Non-empty, trimmed segments of a `::`-separated path, `crate` / `self` /
/// `super` removed (and `Schema` when `strip_schema`).
fn schema_path_segments(path_str: &str, strip_schema: bool) -> Vec<&str> {
    path_str
        .split("::")
        .map(str::trim)
        .filter(|s| !(s.is_empty() || (strip_schema && *s == "Schema")))
        .collect()
}

fn find_in_schema_module(segments: &[&str], struct_name: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");
    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| !matches!(**s, "crate" | "self" | "super"))
        .copied()
        .collect();
    find_struct_in_module(&src_dir, &module_segments, struct_name)
        .map(|definition| StructMetadata::new_model(struct_name.to_string(), definition))
}

/// The crate module (`["crate", ..]`) that holds a `#[derive(Schema)]`
/// storage entry, read from its source identity (`file::Ident@line:col`).
/// `None` without an identity or outside the crate's `src/`.
pub fn storage_module_path(metadata: &StructMetadata) -> Option<Vec<String>> {
    let identity = metadata.source_identity.as_deref()?;
    let (file, _) = identity.rsplit_once('@')?.0.rsplit_once("::")?;
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");
    let file = Path::new(file);
    file.starts_with(&src_dir)
        .then(|| file_path_to_module_path(file, &src_dir))
}

/// The absolute module a qualified source path (`models::user::Model`,
/// `super::user::Model`) names; `None` for a bare name.
pub fn requested_module_path(ty: &Type) -> Option<Vec<String>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|s| crate::parser::strip_raw_prefix_owned(s.ident.to_string()))
        .collect();
    let (_, module_part) = segments.split_last()?;
    if module_part.is_empty() {
        return None;
    }
    let call_site = proc_macro2::Span::call_site().local_file();
    let call_site_module = call_site
        .zip(crate::schema_macro::file_cache::get_manifest_dir())
        .map(|(file, manifest_dir)| {
            file_path_to_module_path(&file, &Path::new(&manifest_dir).join("src"))
        });
    Some(absolute_module_path(
        module_part,
        call_site_module.as_deref(),
    ))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_module_tree;
