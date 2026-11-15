//! Struct lookup/search helpers.

use std::path::{Path, PathBuf};

use syn::Type;

use crate::metadata::StructMetadata;
use crate::schema_macro::crate_layout::{CrateLayout, layout_for};

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
    /// The source module path goes through an import of another crate.
    ExternalCrate { path: String },
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
            Self::ExternalCrate { path } => syn::Error::new_spanned(
                span,
                format!(
                    "schema_type! source module `{path}` is in another crate: only structs of this crate can be read (declare the DTO in that crate, or copy the struct here)"
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
    find_struct_with_call_site(ty, call_site.as_deref(), call_site_line())
}

/// The line the macro is invoked on (it picks the inline `mod` the call
/// sits in, see [`call_site_module_of`]).
fn call_site_line() -> usize {
    proc_macro2::Span::call_site().start().line
}

/// The module a macro invoked on `line` of `file` resolves names in: the
/// file's module plus the inline `mod`s enclosing that line.
fn call_site_module_of(layout: &CrateLayout, file: &Path, line: usize) -> Vec<String> {
    let mut module = layout.module_path_of(file);
    module.extend(crate::schema_macro::module_scope::inline_chain_at(
        file, line,
    ));
    module
}

/// [`find_struct_from_path_detailed`] with the macro call-site file and line
/// passed explicitly (unit tests have none).
pub(super) fn find_struct_with_call_site(
    ty: &Type,
    call_site: Option<&Path>,
    line: usize,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()
        .ok_or(LookupError::MissingManifestDir)?;
    let layout = layout_for(Path::new(&manifest_dir), call_site);
    let src_dir = &layout.module_dir;

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
            Some(file_path) => find_bare_struct_in_file(file_path, &struct_name, &layout, line),
            None => Err(LookupError::BareNotFound { struct_name }),
        };
    }

    let call_site_module = call_site.map(|file| call_site_module_of(&layout, file, line));
    let module_path = absolute_module_path(module_part, call_site_module.as_deref())?;
    let module_segments: Vec<&str> = module_path[1..].iter().map(String::as_str).collect();

    if let Some(definition) = find_struct_in_module(src_dir, &module_segments, &struct_name) {
        return Ok((
            StructMetadata::new_model(struct_name, definition),
            module_path,
        ));
    }
    // `crate::models::User` through `pub use user::User;` (or `pub use
    // user::Model as User;`) in `models`.
    if let Some((defined_in, defined_name)) = reexported_item(&module_path, &struct_name) {
        let segments: Vec<&str> = defined_in[1..].iter().map(String::as_str).collect();
        if let Some(definition) = find_struct_in_module(src_dir, &segments, &defined_name) {
            return Ok((
                StructMetadata::new_model(defined_name, definition),
                defined_in,
            ));
        }
    }

    let mut searched: Vec<PathBuf> = if module_segments.is_empty() {
        Vec::new()
    } else {
        candidate_file_paths(src_dir, &module_segments).into()
    };
    searched.extend(layout.roots.iter().cloned());
    // `entity::user::Model` with no local `entity` module: a dependency
    // crate (the usual SeaORM layout) the compiler links as `--extern`.
    if is_extern_crate(&module_part[0]) {
        return Err(LookupError::ExternalCrate {
            path: module_part.join("::"),
        });
    }
    Err(LookupError::NotFound {
        struct_name,
        searched,
    })
}

/// Whether `name` is a crate the compiler links (`--extern name=..`,
/// options like `priv:` stripped); never outside rustc.
fn is_extern_crate(name: &str) -> bool {
    crate::env_access::compiler_args().windows(2).any(|pair| {
        pair[0] == "--extern" && {
            let spec = pair[1].split('=').next().unwrap_or_default();
            spec.rsplit(':').next() == Some(name)
        }
    })
}

/// The absolute (`crate::..`) module path of a written module path:
/// `crate::a` as is, `self::a` / `super::a` relative to the call-site
/// module (when known), a first segment the call-site module imports or
/// defines (`use crate::models::memo;` → `memo::Model`) through that
/// import, anything else relative to the crate root.  A first segment
/// imported from ANOTHER crate (`use entity::user;` -> `user::Model`) is
/// [`LookupError::ExternalCrate`]: its source is not in this crate.
fn absolute_module_path(
    module_part: &[String],
    call_site_module: Option<&[String]>,
) -> Result<Vec<String>, LookupError> {
    let first = module_part[0].as_str();
    if !matches!(first, "crate" | "self" | "super")
        && let Some(module) = call_site_module
        && let crate::schema_macro::module_scope::Resolved::Path(path) =
            crate::schema_macro::module_scope::resolve_name(&module_part[..1], module)
    {
        let full: Vec<String> = path
            .into_iter()
            .chain(module_part[1..].iter().cloned())
            .collect();
        if full[0] != "crate" {
            return Err(LookupError::ExternalCrate {
                path: full.join("::"),
            });
        }
        return Ok(full);
    }
    Ok(match (first, call_site_module) {
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
    })
}

/// Definition of `struct_name` in the crate module `module_segments`
/// (crate-relative; empty = crate root): the module's own file first (fast,
/// also for trees without a crate root on disk) unless a parent declares
/// the module with `#[path]` / inline, then the module tree.
pub(super) fn find_struct_in_module(
    src_dir: &Path,
    module_segments: &[&str],
    struct_name: &str,
) -> Option<String> {
    let roots = super::super::crate_layout::crate_layout().map(|layout| layout.roots);
    let roots = roots.unwrap_or_default();
    if !module_segments.is_empty()
        && crate::schema_macro::module_scope::fast_path_is_plain(src_dir, &roots, module_segments)
    {
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
    layout: &CrateLayout,
    line: usize,
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    let module_path = layout.module_path_of(file_path);
    let chain = crate::schema_macro::module_scope::inline_chain_at(file_path, line);
    if !chain.is_empty() {
        let module: Vec<String> = module_path.into_iter().chain(chain).collect();
        return find_bare_struct_in_inline_module(struct_name, &module);
    }
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

/// A bare name written inside an inline `mod` (`module`, absolute): that
/// module's own struct, else the one its scope imports (`use super::Model;`)
/// — never a same-named struct of the file's top level, which Rust would not
/// resolve there.
fn find_bare_struct_in_inline_module(
    struct_name: &str,
    module: &[String],
) -> Result<(StructMetadata, Vec<String>), LookupError> {
    use crate::schema_macro::module_scope::{
        Resolved, module_items, resolve_name, struct_in_items,
    };
    let found = |module: Vec<String>| {
        let definition = struct_in_items(&module_items(&module)?, struct_name)?;
        Some((
            StructMetadata::new_model(struct_name.to_string(), definition),
            module,
        ))
    };
    let imported = || match resolve_name(&[struct_name.to_string()], module) {
        Resolved::Path(path)
            if path.first().is_some_and(|root| root == "crate")
                && path.last().is_some_and(|last| last == struct_name) =>
        {
            found(path[..path.len() - 1].to_vec())
        }
        _ => None,
    };
    found(module.to_vec())
        .or_else(imported)
        .ok_or_else(|| LookupError::BareNotFound {
            struct_name: struct_name.to_string(),
        })
}

/// Classifies one already-typed directory entry, keeping the recursive collector's
/// file/dir invariant while accepting scan results as explicit inputs.
#[cfg(test)]
fn collect_rs_entry(
    entry: &std::fs::DirEntry,
    file_type: std::fs::FileType,
    files: &mut Vec<PathBuf>,
) {
    let mut walk = RsWalk {
        files,
        ancestors: std::collections::HashSet::new(),
    };
    walk.entry(entry, file_type, Path::new(""));
}

/// One [`collect_rs_files_recursive`] walk.
struct RsWalk<'a> {
    files: &'a mut Vec<PathBuf>,
    /// Canonical directories on the current recursion path only: a link
    /// back to one of them cannot recurse forever, while a sibling alias of
    /// an already-walked directory is still walked (rustc compiles it).
    ancestors: std::collections::HashSet<PathBuf>,
}

impl RsWalk<'_> {
    /// Walk `dir`, whose canonical path is `canonical`.
    fn dir(&mut self, dir: &Path, canonical: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            // `entry.file_type()` reads the kind from the directory-entry
            // data the OS already returned for this `read_dir` walk — no
            // extra `metadata` stat per entry (only a symlink costs one).
            if let Ok(file_type) = entry.file_type() {
                self.entry(&entry, file_type, canonical);
            }
        }
    }

    /// One entry of the directory whose canonical path is `parent`.
    ///
    /// A symlink is followed like rustc's module loader (and the route
    /// scanner, `file_utils::collect_files_with_mtimes`) does: a symlinked
    /// module directory is part of the crate, so an edit below it must reach
    /// the lookup fingerprint.  A dangling link is skipped.
    fn entry(&mut self, entry: &std::fs::DirEntry, file_type: std::fs::FileType, parent: &Path) {
        let path = entry.path();
        let (is_file, is_dir, canonical) = if file_type.is_symlink() {
            let meta = std::fs::metadata(&path).ok();
            (
                meta.as_ref().is_some_and(std::fs::Metadata::is_file),
                meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
                std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
            )
        } else {
            (
                file_type.is_file(),
                file_type.is_dir(),
                parent.join(entry.file_name()),
            )
        };
        if is_dir {
            if self.ancestors.insert(canonical.clone()) {
                self.dir(&path, &canonical);
                self.ancestors.remove(&canonical);
            }
        } else if is_file && path.extension().is_some_and(|ext| ext == "rs") {
            self.files.push(path);
        }
    }
}

/// Recursively collect all `.rs` files in a directory (symlinks followed).
pub fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut walk = RsWalk {
        files,
        ancestors: std::collections::HashSet::from([canonical.clone()]),
    };
    walk.dir(dir, &canonical);
}

/// Derive module path from a file path relative to the crate's module
/// directory (see [`CrateLayout::module_path_of`]).
///
/// Examples:
/// - `src/models/user.rs` -> `["crate", "models", "user"]`
/// - `src/models/user/mod.rs` -> `["crate", "models", "user"]`
/// - `src/lib.rs` / `src/main.rs` / `src/bin/tool.rs` -> `["crate"]`
/// - a file outside `src_dir` -> `["crate"]`
#[cfg(test)]
pub fn file_path_to_module_path(file_path: &Path, src_dir: &Path) -> Vec<String> {
    CrateLayout {
        module_dir: src_dir.to_path_buf(),
        roots: Vec::new(),
    }
    .module_path_of(file_path)
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

/// Non-empty, trimmed, un-raw'd (`r#type` -> `type`) segments of a
/// `::`-separated path (and without `Schema` when `strip_schema`).
fn schema_path_segments(path_str: &str, strip_schema: bool) -> Vec<&str> {
    path_str
        .split("::")
        .map(|s| s.trim().trim_start_matches("r#"))
        .filter(|s| !(s.is_empty() || (strip_schema && *s == "Schema")))
        .collect()
}

fn find_in_schema_module(segments: &[&str], struct_name: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let layout = crate::schema_macro::crate_layout::crate_layout()?;
    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| !matches!(**s, "crate" | "self" | "super"))
        .copied()
        .collect();
    find_struct_in_module(&layout.module_dir, &module_segments, struct_name)
        .map(|definition| StructMetadata::new_model(struct_name.to_string(), definition))
}

/// The crate module (`["crate", ..]`) that holds a `#[derive(Schema)]`
/// storage entry, read from its source identity (`file::Ident@line:col`):
/// the file's module plus the inline `mod`s the item sits in.
/// `None` without an identity or outside the crate's `src/`.
pub fn storage_module_path(metadata: &StructMetadata) -> Option<Vec<String>> {
    let identity = metadata.source_identity.as_deref()?;
    let (located, position) = identity.rsplit_once('@')?;
    let (file, ident) = located.rsplit_once("::")?;
    let layout = crate::schema_macro::crate_layout::crate_layout()?;
    let file = Path::new(file);
    if !layout.contains(file) {
        return None;
    }
    let mut module = layout.module_path_of(file);
    // Only disambiguates a repeated name: an unreadable line is no line.
    let line = position
        .split(':')
        .next()
        .and_then(|line| line.parse().ok())
        .unwrap_or_default();
    // The file's cached line index (no parse per storage hit).
    if let Some(chain) = crate::schema_macro::module_scope::item_module_chain_cached(
        file,
        ident.trim_start_matches("r#"),
        line,
    ) {
        module.extend(chain);
    }
    Some(module)
}

/// The module a `use` re-export in `module` (absolute, `crate::..`) takes
/// `name` from, under its own name: `pub use user::User;` in
/// `crate::models` gives `crate::models::user` (see [`reexported_item`]).
pub fn reexported_module(module: &[String], name: &str) -> Option<Vec<String>> {
    reexported_item(module, name)
        .filter(|(_, defined)| defined == name)
        .map(|(module, _)| module)
}

/// The module and item name a `use` re-export in `module` (absolute,
/// `crate::..`) takes `name` from: `pub use user::User;` in
/// `crate::models` gives `(crate::models::user, User)`, `pub use
/// user::Model as User;` gives `(crate::models::user, Model)`.  Chained
/// re-exports are followed (a few hops); `None` when `module` does not
/// import `name` from inside the crate.
pub fn reexported_item(module: &[String], name: &str) -> Option<(Vec<String>, String)> {
    let mut current = module.to_vec();
    let mut current_name = name.to_string();
    for _ in 0..8 {
        let crate::schema_macro::module_scope::Resolved::Path(mut path) =
            crate::schema_macro::module_scope::resolve_name(&[current_name.clone()], &current)
        else {
            break;
        };
        let last = path.pop();
        if path.first().map(String::as_str) != Some("crate")
            || (path == current && last.as_deref() == Some(current_name.as_str()))
        {
            break;
        }
        current = path;
        current_name = last.unwrap_or_default();
    }
    (current != module || current_name != name).then_some((current, current_name))
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
        .zip(crate::schema_macro::crate_layout::crate_layout())
        .map(|(file, layout)| call_site_module_of(&layout, &file, call_site_line()));
    absolute_module_path(module_part, call_site_module.as_deref()).ok()
}

/// The call-site file's own definition of the bare source name, when the
/// storage entry `stored` was derived in another file: the struct in scope at
/// a bare `schema_type!(Dto from User)` is the local one, not a same-named
/// `#[derive(Schema)]` of another module.  `None` without a call-site file
/// (unit tests), for an entry of this file, or without a local definition.
pub fn call_site_definition(
    stored: &StructMetadata,
    source: &Type,
    _schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    proc_macro2::Span::call_site()
        .local_file()
        .and_then(|call_site| call_site_definition_in(stored, source, &call_site))
}

/// [`call_site_definition`] with the call-site file passed explicitly.
fn call_site_definition_in(
    stored: &StructMetadata,
    source: &Type,
    call_site: &Path,
) -> Option<(StructMetadata, Vec<String>)> {
    let identity = stored.source_identity.as_deref()?;
    let (located, _) = identity.rsplit_once('@')?;
    let (file, _) = located.rsplit_once("::")?;
    if Path::new(file) == call_site {
        return None;
    }
    find_struct_with_call_site(source, Some(call_site), call_site_line()).ok()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_module_tree;
