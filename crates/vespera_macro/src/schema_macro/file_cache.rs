//! Thread-local cache for file lookups to avoid redundant I/O and parsing.
//!
//! Within a single compilation, multiple `schema_type!` invocations may search
//! for structs in the same files. This module caches:
//! - The list of `.rs` files per source directory
//! - File contents with mtime-based invalidation
//! - Struct name → candidate file paths (cheap text-based pre-filter)
//!
//! Uses `thread_local!` because `syn::File` (and proc-macro types within it)
//! are not `Send`/`Sync`, and proc-macros run single-threaded anyway.
//! The mtime check handles rust-analyzer's proc-macro server, which may persist
//! across file edits.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::circular::CircularAnalysis;
use super::file_lookup::collect_rs_files_recursive;
use crate::metadata::StructMetadata;

/// Internal cache state.
struct FileCache {
    /// Cached `.rs` file lists per source directory.
    file_lists: HashMap<PathBuf, Vec<PathBuf>>,

    /// Cached file contents: file path → (mtime, content string).
    /// Mtime is checked to invalidate stale entries in long-lived processes.
    file_contents: HashMap<PathBuf, (SystemTime, String)>,

    /// Struct name candidate index: (src_dir, struct_name) → files containing that name.
    /// Built from cheap `String::contains` search, not full parsing.
    struct_candidates: HashMap<(PathBuf, String), Vec<PathBuf>>,

    // NOTE: We intentionally do NOT cache parsed `syn::ItemStruct` here.
    // `syn` types contain `proc_macro::Span` handles that are tied to a specific
    // macro invocation context. Caching them across invocations causes
    // "use-after-free in `proc_macro` handle" panics.

    // --- Profiling counters (zero-cost when VESPERA_PROFILE is not set) ---
    /// Number of file content reads from disk (cache miss).
    file_disk_reads: usize,
    /// Number of file content cache hits.
    content_cache_hits: usize,
    /// Number of struct definitions parsed via syn::parse_str.
    struct_parses: usize,
    /// Number of full-file AST parses via syn::parse_file.
    ast_parses: usize,

    // --- Phase 4 caches ---
    /// Cached circular reference analysis results: (module_path, definition) → analysis.
    circular_analysis: HashMap<(String, String), CircularAnalysis>,
    /// Cached struct lookups by schema path: path_str → Option<StructMetadata>.
    /// `None` values are cached (negative cache) to avoid repeated failed lookups.
    struct_lookup: HashMap<String, Option<StructMetadata>>,
    /// Cached FK column lookups: (schema_path, via_rel) → Option<column_name>.
    fk_column_lookup: HashMap<(String, String), Option<String>>,
    /// Cached module path extraction from schema paths: path_str → Vec<module segments>.
    module_path_cache: HashMap<String, Vec<String>>,
    /// Cached CARGO_MANIFEST_DIR value to avoid repeated syscalls.
    /// Within a single compilation, this never changes.
    manifest_dir: Option<String>,

    // --- Phase 4 profiling counters ---
    circular_cache_hits: usize,
    struct_lookup_cache_hits: usize,
    fk_column_cache_hits: usize,
    module_path_cache_hits: usize,
}

thread_local! {
    static FILE_CACHE: RefCell<FileCache> = RefCell::new(FileCache {
        file_lists: HashMap::with_capacity(4),
        file_contents: HashMap::with_capacity(32),
        struct_candidates: HashMap::with_capacity(32),
        file_disk_reads: 0,
        content_cache_hits: 0,
        struct_parses: 0,
        ast_parses: 0,
        circular_analysis: HashMap::with_capacity(16),
        struct_lookup: HashMap::with_capacity(32),
        fk_column_lookup: HashMap::with_capacity(16),
        module_path_cache: HashMap::with_capacity(32),
        manifest_dir: None,
        circular_cache_hits: 0,
        struct_lookup_cache_hits: 0,
        fk_column_cache_hits: 0,
        module_path_cache_hits: 0,
    });
}

/// Get `CARGO_MANIFEST_DIR` from cache, or read from env and cache.
///
/// Within a single compilation, this value never changes. Caching avoids
/// repeated syscalls (previously 20+ calls per `schema_type!` expansion).
pub fn get_manifest_dir() -> Option<String> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(ref dir) = cache.manifest_dir {
            return Some(dir.clone());
        }
        let dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        cache.manifest_dir.clone_from(&dir);
        dir
    })
}

/// Get candidate files that likely contain `struct_name`, using cache when available.
///
/// Performs a cheap text-based search (`String::contains`) on file contents.
/// False positives are acceptable (struct name in comments/strings), but false
/// negatives are not. Results are cached per `(src_dir, struct_name)` pair.
pub fn get_struct_candidates(src_dir: &Path, struct_name: &str) -> Vec<PathBuf> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let key = (src_dir.to_path_buf(), struct_name.to_string());

        if let Some(candidates) = cache.struct_candidates.get(&key) {
            return candidates.clone();
        }

        // Ensure file list is cached
        let files = if let Some(files) = cache.file_lists.get(src_dir) {
            files.clone()
        } else {
            let mut files = Vec::new();
            collect_rs_files_recursive(src_dir, &mut files);
            cache
                .file_lists
                .insert(src_dir.to_path_buf(), files.clone());
            files
        };

        // Filter using cheap text search, caching file contents along the way
        let candidates: Vec<PathBuf> = files
            .into_iter()
            .filter(|path| {
                let content = get_file_content_inner(&mut cache, path);
                content.is_some_and(|c| c.contains(struct_name))
            })
            .collect();

        cache.struct_candidates.insert(key, candidates.clone());
        candidates
    })
}

/// Get a parsed `syn::File` for the given path, using cached file content.
///
/// File content is cached with mtime-based invalidation. Parsing always runs
/// (syn types aren't Send), but I/O is avoided on cache hits.
/// Returns `None` if the file cannot be read or parsed.
pub fn get_parsed_ast(path: &Path) -> Option<syn::File> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let content = get_file_content_inner(&mut cache, path)?;
        cache.ast_parses += 1;
        syn::parse_file(&content).ok()
    })
}

/// Internal helper: get file content from cache or read from disk.
/// Checks mtime for invalidation.
fn get_file_content_inner(cache: &mut FileCache, path: &Path) -> Option<String> {
    let current_mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());

    if let Some(mtime) = current_mtime
        && let Some((cached_mtime, content)) = cache.file_contents.get(path)
        && *cached_mtime == mtime
    {
        cache.content_cache_hits += 1;
        return Some(content.clone());
    }

    // Cache miss or stale — read and cache
    let content = std::fs::read_to_string(path).ok()?;
    cache.file_disk_reads += 1;

    if let Some(mtime) = current_mtime {
        cache
            .file_contents
            .insert(path.to_path_buf(), (mtime, content.clone()));
    }

    Some(content)
}

/// Parse a struct definition string via `syn::parse_str`.
///
/// NOTE: Results are NOT cached across calls. `syn::ItemStruct` contains
/// `proc_macro::Span` handles that are tied to a specific macro invocation
/// context — caching them causes "use-after-free" panics in the proc_macro bridge.
/// File I/O caching (via `get_parsed_ast`) is the primary performance win;
/// definition string parsing is fast (microseconds per struct).
pub fn parse_struct_cached(definition: &str) -> Result<syn::ItemStruct, syn::Error> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.struct_parses += 1;
        syn::parse_str(definition)
    })
}

/// Get or compute circular reference analysis, with caching.
///
/// The cache key is `(source_module_path_joined, definition)` since the same
/// model definition analyzed from the same module context always produces
/// the same result.
pub fn get_circular_analysis(source_module_path: &[String], definition: &str) -> CircularAnalysis {
    let key = (source_module_path.join("::"), definition.to_string());

    // 1. Check cache — borrow dropped at end of closure
    let cached = FILE_CACHE.with(|cache| cache.borrow().circular_analysis.get(&key).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().circular_cache_hits += 1);
        return result;
    }

    // 2. Compute — this re-enters FILE_CACHE via parse_struct_cached (safe: our borrow is dropped)
    let result = super::circular::analyze_circular_refs(source_module_path, definition);

    // 3. Store — new borrow
    FILE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .circular_analysis
            .insert(key, result.clone());
    });

    result
}

/// Get or compute struct lookup by schema path, with caching.
///
/// Wraps `find_struct_from_schema_path` with a `HashMap<String, Option<StructMetadata>>`
/// cache. `None` values are cached too (negative cache) to avoid repeated failed lookups.
pub fn get_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    // 1. Check cache — borrow dropped at end of closure
    let cached = FILE_CACHE.with(|cache| cache.borrow().struct_lookup.get(path_str).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().struct_lookup_cache_hits += 1);
        return result;
    }

    // 2. Compute — this re-enters FILE_CACHE via get_parsed_ast (safe: our borrow is dropped)
    let result = super::file_lookup::find_struct_from_schema_path(path_str);

    // 3. Store — new borrow
    FILE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .struct_lookup
            .insert(path_str.to_string(), result.clone());
    });

    result
}

/// Get or compute FK column lookup, with caching.
///
/// Wraps `find_fk_column_from_target_entity` with a `HashMap<(String, String), Option<String>>`
/// cache. Negative results (`None`) are cached to avoid repeated file lookups.
pub fn get_fk_column(schema_path: &str, via_rel: &str) -> Option<String> {
    let key = (schema_path.to_string(), via_rel.to_string());

    // 1. Check cache — borrow dropped at end of closure
    let cached = FILE_CACHE.with(|cache| cache.borrow().fk_column_lookup.get(&key).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().fk_column_cache_hits += 1);
        return result;
    }

    // 2. Compute — this re-enters FILE_CACHE via get_parsed_ast (safe: our borrow is dropped)
    let result = super::file_lookup::find_fk_column_from_target_entity(schema_path, via_rel);

    // 3. Store — new borrow
    FILE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .fk_column_lookup
            .insert(key, result.clone());
    });

    result
}

/// Get or compute module path from schema path, with caching.
///
/// Wraps `extract_module_path_from_schema_path` logic with a `HashMap<String, Vec<String>>`
/// cache. The `schema_path` TokenStream is stringified once for both cache key and computation,
/// avoiding the double `.to_string()` that would occur when calling the uncached function.
pub fn get_module_path_from_schema_path(schema_path: &proc_macro2::TokenStream) -> Vec<String> {
    let path_str = schema_path.to_string();

    // 1. Check cache — borrow dropped at end of closure
    let cached = FILE_CACHE.with(|cache| cache.borrow().module_path_cache.get(&path_str).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().module_path_cache_hits += 1);
        return result;
    }

    // 2. Compute from the string directly (avoids double to_string())
    let segments: Vec<&str> = path_str
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let result = if segments.len() > 1 {
        segments[..segments.len() - 1]
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        vec![]
    };

    // 3. Store — new borrow
    FILE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .module_path_cache
            .insert(path_str, result.clone());
    });

    result
}

/// Print profiling summary to stderr if `VESPERA_PROFILE` env var is set.
///
/// Call this at the end of macro execution to output cache statistics.
/// Silent by default — only outputs when `VESPERA_PROFILE=1`.
pub fn print_profile_summary() {
    if std::env::var("VESPERA_PROFILE").is_err() {
        return;
    }

    FILE_CACHE.with(|cache| {
        let cache = cache.borrow();
        eprintln!("[vespera-profile] File cache stats:");
        eprintln!(
            "  file I/O: {} disk reads, {} cache hits",
            cache.file_disk_reads, cache.content_cache_hits
        );
        eprintln!("  struct parses: {}", cache.struct_parses);
        eprintln!("  AST parses: {}", cache.ast_parses);
        eprintln!(
            "  cache entries: {} file lists, {} file contents, {} struct candidates",
            cache.file_lists.len(),
            cache.file_contents.len(),
            cache.struct_candidates.len()
        );
        eprintln!(
            "  circular analysis: {} cache hits, {} entries",
            cache.circular_cache_hits,
            cache.circular_analysis.len()
        );
        eprintln!(
            "  struct lookup: {} cache hits, {} entries",
            cache.struct_lookup_cache_hits,
            cache.struct_lookup.len()
        );
        eprintln!(
            "  FK column lookup: {} cache hits, {} entries",
            cache.fk_column_cache_hits,
            cache.fk_column_lookup.len()
        );
        eprintln!(
            "  module path: {} cache hits, {} entries",
            cache.module_path_cache_hits,
            cache.module_path_cache.len()
        );
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_get_struct_candidates_filters_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::write(
            src_dir.join("has_model.rs"),
            "pub struct Model { pub id: i32 }",
        )
        .unwrap();
        std::fs::write(
            src_dir.join("no_model.rs"),
            "pub struct Other { pub x: i32 }",
        )
        .unwrap();

        let candidates = get_struct_candidates(src_dir, "Model");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].ends_with("has_model.rs"));
    }

    #[test]
    fn test_get_parsed_ast_returns_valid_ast() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "pub struct Foo { pub x: i32 }").unwrap();

        let ast = get_parsed_ast(&file_path);
        assert!(ast.is_some());
        assert!(!ast.unwrap().items.is_empty());
    }

    #[test]
    fn test_get_parsed_ast_caches_content() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("cached.rs");
        std::fs::write(&file_path, "pub struct Bar;").unwrap();

        let ast1 = get_parsed_ast(&file_path);
        let ast2 = get_parsed_ast(&file_path);
        assert!(ast1.is_some());
        assert!(ast2.is_some());
    }

    #[test]
    fn test_get_parsed_ast_returns_none_for_invalid() {
        let result = get_parsed_ast(Path::new("/nonexistent/path.rs"));
        assert!(result.is_none());
    }

    #[test]
    fn test_get_parsed_ast_returns_none_for_unparseable() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("broken.rs");
        std::fs::write(&file_path, "this is not valid rust {{{{").unwrap();

        let result = get_parsed_ast(&file_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_struct_candidates_caches_result() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::write(src_dir.join("file.rs"), "pub struct Target { pub id: i32 }").unwrap();

        let c1 = get_struct_candidates(src_dir, "Target");
        let c2 = get_struct_candidates(src_dir, "Target");
        assert_eq!(c1, c2, "Cached candidates should be identical");
    }

    #[test]
    fn test_get_struct_candidates_file_list_cache_hit() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::write(
            src_dir.join("file_a.rs"),
            "pub struct Alpha { pub id: i32 }",
        )
        .unwrap();
        std::fs::write(
            src_dir.join("file_b.rs"),
            "pub struct Beta { pub name: String }",
        )
        .unwrap();

        // First call: populates file_lists cache for src_dir
        let result1 = get_struct_candidates(src_dir, "Alpha");
        assert_eq!(result1.len(), 1);

        // Second call: same src_dir, different struct_name
        // struct_candidates cache MISS (different key), but file_lists cache HIT → line 125
        let result2 = get_struct_candidates(src_dir, "Beta");
        assert_eq!(result2.len(), 1);
    }

    #[test]
    fn test_get_fk_column_cache_hit() {
        // First call: computes and caches result (None since path doesn't exist)
        let result1 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
        // Second call: hits cache → lines 259-260
        let result2 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
        assert_eq!(result1, result2);
    }

    #[serial_test::serial]
    #[test]
    fn test_print_profile_summary_with_profile_env() {
        // Set VESPERA_PROFILE to enable profiling output
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        // This should print profile summary to stderr (lines 311-321)
        print_profile_summary();

        // Clean up
        unsafe { std::env::remove_var("VESPERA_PROFILE") };
        // Test passes if no panic — output goes to stderr
    }

    #[serial_test::serial]
    #[test]
    fn test_print_profile_summary_without_profile_env() {
        // Ensure VESPERA_PROFILE is not set
        unsafe { std::env::remove_var("VESPERA_PROFILE") };

        // Should early-return at line 308 without printing anything
        print_profile_summary();
    }
}
