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
//!
//! ## Epoch caching
//!
//! `fs::metadata` costs ~1–10 µs per call. Projects with 100+ source files
//! previously paid that cost on every cache lookup, even on hits.
//!
//! The epoch mechanism amortises this: each top-level macro invocation
//! (`vespera!`, `schema_type!`) calls [`bump_epoch`] once at entry. Within
//! that epoch, a given path's mtime is fetched from `fs::metadata` **at most
//! once** and stored in `mtime_epoch_cache`. Subsequent lookups for the same
//! path in the same epoch reuse the cached mtime without a syscall.
//!
//! Across epochs the full mtime check still runs, preserving the existing
//! invalidation semantics (important for rust-analyzer's long-lived server).

use std::cell::RefCell;
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

// Test-only thread-local counter: number of `fs::metadata` calls made on
// this thread. Thread-local so parallel test threads don't interfere with
// each other's counts.
#[cfg(test)]
thread_local! {
    static METADATA_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reset the test-only metadata call counter to zero for this thread.
#[cfg(test)]
pub fn reset_metadata_call_count() {
    METADATA_CALL_COUNT.with(|c| c.set(0));
}

/// Return the current value of the test-only metadata call counter for this thread.
#[cfg(test)]
pub fn metadata_call_count() -> usize {
    METADATA_CALL_COUNT.with(std::cell::Cell::get)
}

use super::circular::CircularAnalysis;
use super::file_lookup::collect_rs_files_recursive;
use crate::metadata::StructMetadata;

/// Cached directory walk for a single `src_dir`.
///
/// `fingerprint` is a SipHash over the sorted `(path, mtime)` pairs of
/// every `.rs` file under the directory. Within the same macro
/// invocation (matched via `last_epoch_validated == cache.epoch`) the
/// entry is trusted without rewalking; across invocations the directory
/// is rewalked once and the fingerprint comparison decides whether the
/// cached `files` (and the dependent `struct_index`) stay live.
///
/// Replaces the prior bare `Arc<[PathBuf]>` cache, which silently
/// missed `.rs` files added in long-lived rust-analyzer proc-macro
/// servers.
#[derive(Clone)]
struct DirEntry {
    fingerprint: u64,
    last_epoch_validated: u64,
    files: Arc<[PathBuf]>,
}

/// Internal cache state.
struct FileCache {
    /// Cached `.rs` file lists per source directory with a directory
    /// fingerprint for cross-invocation invalidation.
    ///
    /// See [`DirEntry`] for the invalidation semantics.
    file_lists: HashMap<PathBuf, DirEntry>,

    /// Cached file contents: file path → (mtime, content string).
    /// Mtime is checked to invalidate stale entries in long-lived processes.
    ///
    /// `Arc<String>` lets the cache hand out cheap pointer-clones instead of
    /// copying the entire file body on every lookup.  The previous `String`
    /// variant cloned `O(file_size)` bytes per cache hit and a second time
    /// on insert; both become single-word `Arc::clone`s.
    file_contents: HashMap<PathBuf, (SystemTime, Arc<String>)>,

    /// Per-`src_dir` struct identifier index: struct name → files that
    /// define it (as a top-level `struct <Name>` declaration found via
    /// cheap source-text tokenisation in [`extract_struct_names`]).
    ///
    /// Built lazily on the first `get_struct_candidates` call for a
    /// directory; dropped alongside its `file_lists` entry whenever the
    /// directory fingerprint changes.
    ///
    /// Replaces the prior per-`(src_dir, name)` full-source
    /// `String::contains` scan (`struct_candidates`), which was
    /// O(N×M) for N struct lookups across M files. The index is O(M)
    /// tokenisation passes to build, then O(1) per lookup.
    struct_index: HashMap<PathBuf, HashMap<String, Arc<[PathBuf]>>>,

    // NOTE: We CANNOT cache `syn::File` or `syn::ItemStruct` across proc-macro
    // invocations. Both `syn` and `proc_macro2` types contain `proc_macro::Span`
    // and `proc_macro::TokenStream` bridge handles allocated in the current
    // invocation's bridge context. Cloning them in a later invocation panics with
    // "use-after-free in `proc_macro` handle".
    //
    // Instead, `struct_definitions` caches extracted definition *strings* which have
    // no bridge handles and are safe to reuse. For callers needing `syn::File`,
    // `get_parsed_file()` caches the file *content* (safe string) and re-parses
    // per invocation, avoiding redundant disk I/O while staying safe.

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
    /// Cached struct lookups by schema path: path_str → Option<Arc<StructMetadata>>.
    /// `None` values are cached (negative cache) to avoid repeated failed lookups.
    /// `Arc` because `StructMetadata.definition` holds the full struct
    /// source text — cloning it per hit copied kilobytes.
    struct_lookup: HashMap<String, Option<Arc<StructMetadata>>>,
    /// Cached FK column lookups: (schema_path, via_rel) → Option<column_name>.
    fk_column_lookup: HashMap<(String, String), Option<String>>,
    /// Cached module path extraction from schema paths: path_str → Vec<module segments>.
    module_path_cache: HashMap<String, Vec<String>>,
    /// Cached struct definitions from files: file_path → (mtime, struct_name → definition_string).
    /// Unlike `syn::File`, strings have no `proc_macro::Span` handles, safe to cache.
    struct_definitions: HashMap<PathBuf, (SystemTime, HashMap<String, String>)>,
    /// Cached CARGO_MANIFEST_DIR value to avoid repeated syscalls.
    /// Within a single compilation, this never changes.
    manifest_dir: Option<String>,

    // --- Phase 4 profiling counters ---
    circular_cache_hits: usize,
    struct_lookup_cache_hits: usize,
    fk_column_cache_hits: usize,
    module_path_cache_hits: usize,
    struct_def_cache_hits: usize,

    // --- Epoch caching ---
    /// Monotonically increasing counter. Bumped once at the start of each
    /// top-level macro invocation (`vespera!`, `schema_type!`).
    epoch: u64,
    /// Per-epoch mtime cache: path → (epoch_when_checked, mtime_result).
    ///
    /// When the stored epoch equals `self.epoch`, the mtime was already
    /// fetched during this invocation and `fs::metadata` is skipped.
    /// When the epoch differs the entry is stale and the syscall runs again.
    mtime_epoch_cache: HashMap<PathBuf, (u64, Option<SystemTime>)>,
}

thread_local! {
    static FILE_CACHE: RefCell<FileCache> = RefCell::new(FileCache {
        file_lists: HashMap::with_capacity(4),
        file_contents: HashMap::with_capacity(32),
        struct_index: HashMap::with_capacity(4),
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
        struct_definitions: HashMap::with_capacity(32),
        struct_def_cache_hits: 0,
        epoch: 0,
        mtime_epoch_cache: HashMap::with_capacity(32),
    });
}

/// Advance the per-invocation epoch counter.
///
/// Call this **once** at the start of each top-level macro invocation
/// (`vespera!`, `schema_type!`). Within a single epoch, `fs::metadata` is
/// called at most once per path; subsequent lookups for the same path reuse
/// the cached mtime without a syscall.
///
/// Across epochs the full mtime check still runs, preserving the existing
/// invalidation semantics for long-lived processes (e.g. rust-analyzer).
pub fn bump_epoch() {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.epoch = cache.epoch.wrapping_add(1);
    });
}

/// Fetch the mtime for `path`, using the epoch cache to avoid redundant
/// `fs::metadata` syscalls within a single macro invocation.
///
/// Returns `None` if the file does not exist or its mtime is unavailable.
fn get_mtime_cached(cache: &mut FileCache, path: &Path) -> Option<SystemTime> {
    let current_epoch = cache.epoch;
    if let Some(&(entry_epoch, mtime)) = cache.mtime_epoch_cache.get(path)
        && entry_epoch == current_epoch
    {
        return mtime;
    }
    #[cfg(test)]
    METADATA_CALL_COUNT.with(|c| c.set(c.get() + 1));
    let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    cache
        .mtime_epoch_cache
        .insert(path.to_path_buf(), (current_epoch, mtime));
    mtime
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

/// Get a parsed `syn::File` for the given path.
///
/// Uses the file content cache to avoid redundant disk I/O, then parses with
/// `syn::parse_file` each time. We CANNOT cache `syn::File` across proc-macro
/// invocations because `proc_macro2`/`syn` types contain `proc_macro::TokenStream`
/// bridge handles that become invalid when the invocation that created them ends.
pub fn get_parsed_file(path: &Path) -> Option<syn::File> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        parse_file_cached(&mut cache, path)
    })
}

/// **Single call site for `syn::parse_file`.**
///
/// Reads file content from the mtime-validated content cache (avoids redundant
/// disk I/O), then calls `syn::parse_file`. The resulting `syn::File` is NOT
/// cached — it must be used and dropped within the current proc-macro invocation.
fn parse_file_cached(cache: &mut FileCache, path: &Path) -> Option<syn::File> {
    let content = get_file_content_inner(cache, path)?;
    cache.ast_parses += 1;
    syn::parse_file(&content).ok()
}

/// Walk every `.rs` file under `dir` and produce a content-stable
/// fingerprint of `(sorted path, mtime)` pairs.
///
/// The fingerprint is a `DefaultHasher` (SipHash) digest computed in
/// path-sorted order so it is determinstic and stable across runs.  It
/// changes iff a `.rs` file under `dir` is added, removed, or modified
/// in a way that perturbs its mtime — which is exactly the trigger we
/// need to invalidate the cached file list and the dependent struct
/// identifier index.
///
/// `mtime` lookups reuse the per-epoch [`get_mtime_cached`] so this is
/// effectively one `fs::metadata` per file per epoch, and zero subsequent
/// `fs::metadata` calls for the same path within the same epoch.
fn walk_and_fingerprint(cache: &mut FileCache, dir: &Path) -> (Vec<PathBuf>, u64) {
    let mut files = Vec::new();
    collect_rs_files_recursive(dir, &mut files);
    files.sort();

    let mut hasher = DefaultHasher::new();
    for path in &files {
        path.hash(&mut hasher);
        if let Some(mtime) = get_mtime_cached(cache, path)
            && let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH)
        {
            duration.as_secs().hash(&mut hasher);
            duration.subsec_nanos().hash(&mut hasher);
        }
    }
    (files, hasher.finish())
}

/// Validate (or build) the [`DirEntry`] for `src_dir` and return its file list.
///
/// * Same epoch (`last_epoch_validated == cache.epoch`) → trust cache,
///   no rewalk, no `fs::metadata` calls — pure `Arc::clone`.
/// * New epoch, identical fingerprint → refresh `last_epoch_validated`
///   to suppress further work in the rest of the epoch; cached
///   [`FileCache::struct_index`] entry stays live.
/// * New epoch, different fingerprint → drop the dependent
///   [`FileCache::struct_index`] entry; install a fresh `DirEntry`.
fn ensure_file_list(cache: &mut FileCache, src_dir: &Path) -> Arc<[PathBuf]> {
    let current_epoch = cache.epoch;

    if let Some(entry) = cache.file_lists.get(src_dir)
        && entry.last_epoch_validated == current_epoch
    {
        return Arc::clone(&entry.files);
    }

    let (files_vec, fp) = walk_and_fingerprint(cache, src_dir);

    if let Some(entry) = cache.file_lists.get(src_dir) {
        if entry.fingerprint == fp {
            let files = Arc::clone(&entry.files);
            cache.file_lists.insert(
                src_dir.to_path_buf(),
                DirEntry {
                    fingerprint: fp,
                    last_epoch_validated: current_epoch,
                    files: Arc::clone(&files),
                },
            );
            return files;
        }
        // Directory changed: the dependent index is now stale.
        cache.struct_index.remove(src_dir);
    }

    let files: Arc<[PathBuf]> = files_vec.into();
    cache.file_lists.insert(
        src_dir.to_path_buf(),
        DirEntry {
            fingerprint: fp,
            last_epoch_validated: current_epoch,
            files: Arc::clone(&files),
        },
    );
    files
}

/// Cheap source-text tokeniser: extract every `struct <Name>` identifier
/// from `content`.
///
/// Splits on the standard Rust identifier-character class and walks the
/// resulting token stream looking for the literal `struct` followed by
/// a valid identifier.  This is intentionally lighter than `syn::parse_file`
/// — false positives in comments or strings are acceptable (the eventual
/// [`get_struct_definition`] still does the exact match), but `struct`
/// keywords inside string literals are exceedingly rare in real source
/// and false negatives are not possible for any actually-defined struct.
fn extract_struct_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut tokens = content
        .split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty());

    while let Some(token) = tokens.next() {
        if token == "struct"
            && let Some(name) = tokens.next()
            && name
                .chars()
                .next()
                .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        {
            names.push(name.to_string());
        }
    }

    names
}

/// Get candidate files that likely contain `struct_name`.
///
/// Uses the per-`src_dir` struct identifier index built lazily on first
/// access.  Once built, subsequent lookups for *any* struct name under
/// the same `src_dir` are O(1) — replacing the prior per-name
/// full-source `String::contains` scan (O(N×M) for N lookups across
/// M files).
///
/// The index lives alongside the directory fingerprint in
/// [`FileCache::file_lists`]; both are dropped together whenever the
/// fingerprint changes (file added/removed/modified), so newly added
/// `.rs` files become visible after the next `bump_epoch` in long-lived
/// rust-analyzer servers.
pub fn get_struct_candidates(src_dir: &Path, struct_name: &str) -> Arc<[PathBuf]> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();

        // Validate / build the `.rs` file list under fingerprint control
        // (handles ADD/REMOVE/MODIFY invalidation across epochs).
        let files = ensure_file_list(&mut cache, src_dir);

        // Build the per-src_dir struct identifier index on first miss.
        // Subsequent calls for any name under the same src_dir short
        // circuit to an O(1) lookup.
        if !cache.struct_index.contains_key(src_dir) {
            let mut grouped: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for path in files.iter() {
                let Some(content) = get_file_content_inner(&mut cache, path) else {
                    continue;
                };
                for name in extract_struct_names(&content) {
                    grouped.entry(name).or_default().push(path.clone());
                }
            }
            let index: HashMap<String, Arc<[PathBuf]>> = grouped
                .into_iter()
                .map(|(name, paths)| (name, paths.into()))
                .collect();
            cache.struct_index.insert(src_dir.to_path_buf(), index);
        }

        cache
            .struct_index
            .get(src_dir)
            .and_then(|idx| idx.get(struct_name).cloned())
            .unwrap_or_else(|| Vec::<PathBuf>::new().into())
    })
}
/// Ensure struct definitions are extracted and cached for the given file.
/// On first call, parses the file and caches all struct definitions as strings.
/// On subsequent calls, checks mtime to validate cache.
fn ensure_struct_definitions(cache: &mut FileCache, path: &Path) -> bool {
    let current_mtime = get_mtime_cached(cache, path);

    if let Some(mtime) = current_mtime
        && let Some((cached_mtime, _)) = cache.struct_definitions.get(path)
        && *cached_mtime == mtime
    {
        cache.struct_def_cache_hits += 1;
        return true;
    }

    let Some(file_ast) = parse_file_cached(cache, path) else {
        return false;
    };

    let mut defs = HashMap::new();
    for item in &file_ast.items {
        if let syn::Item::Struct(struct_item) = item {
            let name = struct_item.ident.to_string();
            let def = quote::quote!(#struct_item).to_string();
            defs.insert(name, def);
        }
    }

    if let Some(mtime) = current_mtime {
        cache
            .struct_definitions
            .insert(path.to_path_buf(), (mtime, defs));
    }

    true
}

/// Get a struct definition string by name from a file, using cached extraction.
///
/// On first call for a file, parses via `syn::parse_file` and caches ALL struct
/// definitions as strings. Subsequent calls for the same file return from cache
/// without re-parsing.
///
/// The cached data contains no `proc_macro::Span` handles,
/// so it's safe to reuse across macro invocations.
pub fn get_struct_definition(path: &Path, struct_name: &str) -> Option<String> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !ensure_struct_definitions(&mut cache, path) {
            return None;
        }
        cache
            .struct_definitions
            .get(path)?
            .1
            .get(struct_name)
            .cloned()
    })
}

/// Internal helper: get file content from cache or read from disk.
/// Checks mtime for invalidation.
///
/// Returns `Arc<String>` so callers share a single allocation instead of
/// cloning the whole file body per lookup.
fn get_file_content_inner(cache: &mut FileCache, path: &Path) -> Option<Arc<String>> {
    let current_mtime = get_mtime_cached(cache, path);

    if let Some(mtime) = current_mtime
        && let Some((cached_mtime, content)) = cache.file_contents.get(path)
        && *cached_mtime == mtime
    {
        cache.content_cache_hits += 1;
        return Some(Arc::clone(content));
    }

    let content = Arc::new(std::fs::read_to_string(path).ok()?);
    cache.file_disk_reads += 1;

    if let Some(mtime) = current_mtime {
        cache
            .file_contents
            .insert(path.to_path_buf(), (mtime, Arc::clone(&content)));
    }

    Some(content)
}

/// Parse a struct definition string via `syn::parse_str`.
///
/// NOTE: Results are NOT cached across calls. `syn::ItemStruct` contains
/// `proc_macro::Span` handles that are tied to a specific macro invocation
/// context — caching them causes "use-after-free" panics in the proc_macro bridge.
/// File I/O caching (via `get_struct_definition`) is the primary performance win;
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

    // The borrow must end before analyzing: analysis re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| cache.borrow().circular_analysis.get(&key).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().circular_cache_hits += 1);
        return result;
    }

    let result = super::circular::analyze_circular_refs(source_module_path, definition);

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
/// Wraps `find_struct_from_schema_path` with a
/// `HashMap<String, Option<Arc<StructMetadata>>>` cache. `None` values
/// are cached too (negative cache) to avoid repeated failed lookups.
/// The `Arc` makes cache hits O(1) instead of cloning the full struct
/// definition text per lookup.
pub fn get_struct_from_schema_path(path_str: &str) -> Option<Arc<StructMetadata>> {
    // The borrow must end before lookup: lookup re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| cache.borrow().struct_lookup.get(path_str).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().struct_lookup_cache_hits += 1);
        return result;
    }

    let result = super::file_lookup::find_struct_from_schema_path(path_str).map(Arc::new);

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

    // The borrow must end before lookup: lookup re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| cache.borrow().fk_column_lookup.get(&key).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().fk_column_cache_hits += 1);
        return result;
    }

    let result = super::file_lookup::find_fk_column_from_target_entity(schema_path, via_rel);

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

    let cached = FILE_CACHE.with(|cache| cache.borrow().module_path_cache.get(&path_str).cloned());
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().module_path_cache_hits += 1);
        return result;
    }

    let mut result: Vec<String> = path_str
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    result.pop();

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
            "  cache entries: {} file lists, {} file contents, {} struct index dirs",
            cache.file_lists.len(),
            cache.file_contents.len(),
            cache.struct_index.len()
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
            "  struct definitions: {} cache hits, {} entries",
            cache.struct_def_cache_hits,
            cache.struct_definitions.len()
        );
        eprintln!(
            "  module path: {} cache hits, {} entries",
            cache.module_path_cache_hits,
            cache.module_path_cache.len()
        );
    });
}

/// Inject a fake struct definition into the cache for testing.
/// Uses the file's real mtime so `ensure_struct_definitions` won't invalidate the cache.
/// Enables tests to simulate scenarios where `get_struct_definition` succeeds
/// but `parse_struct_cached` fails (defensive code path).
#[cfg(test)]
pub fn inject_struct_definition_for_test(path: &std::path::Path, name: &str, definition: &str) {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let mtime = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let entry = cache
            .struct_definitions
            .entry(path.to_path_buf())
            .or_insert_with(|| (mtime, HashMap::new()));
        entry.0 = mtime;
        entry.1.insert(name.to_string(), definition.to_string());
    });
}

#[cfg(test)]
mod tests {

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

        let result1 = get_struct_candidates(src_dir, "Alpha");
        assert_eq!(result1.len(), 1);

        let result2 = get_struct_candidates(src_dir, "Beta");
        assert_eq!(result2.len(), 1);
    }

    #[test]
    fn test_get_fk_column_cache_hit() {
        let result1 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
        let result2 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
        assert_eq!(result1, result2);
    }

    #[serial_test::serial]
    #[test]
    fn test_print_profile_summary_with_profile_env() {
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        print_profile_summary();

        unsafe { std::env::remove_var("VESPERA_PROFILE") };
    }

    #[serial_test::serial]
    #[test]
    fn test_print_profile_summary_without_profile_env() {
        unsafe { std::env::remove_var("VESPERA_PROFILE") };

        print_profile_summary();
    }

    /// Verify that within one epoch a path's mtime is checked via `fs::metadata`
    /// exactly once, and that bumping the epoch causes a re-check.
    ///
    /// Layout:
    ///   epoch N  → read path twice → 1 metadata call (second read hits epoch cache)
    ///   bump     → epoch N+1
    ///   epoch N+1 → read path once → 1 more metadata call (epoch cache stale)
    ///
    /// Total expected: 2 metadata calls for 3 reads across 2 epochs.
    #[serial_test::serial]
    #[test]
    fn test_epoch_skips_metadata_syscall() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("target.rs");
        std::fs::write(&file_path, "pub struct Foo { pub x: i32 }").unwrap();

        // Reset the global counter and start a fresh epoch so this test is
        // independent of whatever other tests ran on this thread before.
        reset_metadata_call_count();
        bump_epoch();

        let before = metadata_call_count();

        // First read in epoch N — must call fs::metadata (epoch cache miss).
        let c1 = get_struct_definition(&file_path, "Foo");
        assert!(c1.is_some(), "struct should be found");
        assert_eq!(
            metadata_call_count() - before,
            1,
            "first read should trigger exactly 1 metadata call"
        );

        // Second read in epoch N — epoch cache hit, no additional metadata call.
        let c2 = get_struct_definition(&file_path, "Foo");
        assert_eq!(c1, c2);
        assert_eq!(
            metadata_call_count() - before,
            1,
            "second read in same epoch must NOT call metadata again"
        );

        // Advance to epoch N+1.
        bump_epoch();

        // First read in epoch N+1 — epoch cache is stale, must re-check metadata.
        let c3 = get_struct_definition(&file_path, "Foo");
        assert_eq!(c1, c3);
        assert_eq!(
            metadata_call_count() - before,
            2,
            "read after epoch bump must call metadata exactly once more"
        );
    }

    /// Verify cross-entry invalidation semantics.
    ///
    /// In a long-lived rust-analyzer proc-macro server the same thread handles
    /// multiple successive macro invocations.  Each entry point (`derive_schema`,
    /// `schema_type!`, `schema!`, `export_app!`, `vespera!`) calls `bump_epoch()`
    /// as its first statement.  This test simulates two successive invocations
    /// from *different* entry points and confirms that:
    ///
    /// 1. Within invocation A (epoch N): path checked once, second access free.
    /// 2. Invocation B starts (epoch N+1 via bump): path re-checked exactly once.
    /// 3. Within invocation B: second access still free.
    ///
    /// The test uses `bump_epoch()` directly (the same call each entry point
    /// makes) so it exercises the exact mechanism without needing a real
    /// proc-macro expansion.
    ///
    /// NOTE: `bump_epoch()` is the *only* mechanism that separates invocations;
    /// the call sites in lib.rs are the authoritative hook locations:
    ///   - `derive_schema`  → reaches file_cache via extract_field_defaults_from_path
    ///   - `schema`         → reaches file_cache via parse_struct_cached
    ///   - `schema_type!`   → reaches file_cache via generate_schema_type_code
    ///   - `export_app!`    → reaches file_cache via collect_metadata
    ///   - `vespera!`       → reaches file_cache via collect_metadata
    #[serial_test::serial]
    #[test]
    fn test_epoch_cross_entry_invalidation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("cross.rs");
        std::fs::write(&file_path, "pub struct Bar { pub y: u64 }").unwrap();

        reset_metadata_call_count();

        // ── Invocation A (simulates e.g. derive_schema entry) ──────────────
        bump_epoch(); // what every entry point does first
        let before_a = metadata_call_count();

        let r1 = get_struct_definition(&file_path, "Bar");
        assert!(r1.is_some());
        assert_eq!(
            metadata_call_count() - before_a,
            1,
            "invocation A: first access must call metadata once"
        );

        // Second access within the same invocation — epoch cache hit.
        let r2 = get_struct_definition(&file_path, "Bar");
        assert_eq!(r1, r2);
        assert_eq!(
            metadata_call_count() - before_a,
            1,
            "invocation A: second access must NOT call metadata again"
        );

        // ── Invocation B (simulates e.g. schema_type! entry) ───────────────
        bump_epoch(); // new invocation → new epoch
        let before_b = metadata_call_count();

        // First access in invocation B — epoch cache stale, must re-check.
        let r3 = get_struct_definition(&file_path, "Bar");
        assert_eq!(r1, r3);
        assert_eq!(
            metadata_call_count() - before_b,
            1,
            "invocation B: first access must re-check metadata (cross-entry invalidation)"
        );

        // Second access within invocation B — epoch cache hit again.
        let r4 = get_struct_definition(&file_path, "Bar");
        assert_eq!(r1, r4);
        assert_eq!(
            metadata_call_count() - before_b,
            1,
            "invocation B: second access must NOT call metadata again"
        );
    }

    /// Regression test for the original [`FileCache::file_lists`] bug: a
    /// `.rs` file added to a `src_dir` between two epochs must become
    /// visible to `get_struct_candidates` after the next [`bump_epoch`],
    /// because the directory fingerprint changes.
    ///
    /// In the pre-fix world the file list was cached forever per `src_dir`
    /// with no invalidation mechanism — long-lived rust-analyzer servers
    /// silently missed newly added files. This test would have hit the
    /// 0-length assertion on the post-bump query.
    #[serial_test::serial]
    #[test]
    fn test_struct_index_invalidates_when_new_file_added() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();

        std::fs::write(src_dir.join("first.rs"), "pub struct First { pub id: i32 }").unwrap();

        bump_epoch();
        let first = get_struct_candidates(src_dir, "First");
        assert_eq!(first.len(), 1, "first.rs must be picked up");
        let missing = get_struct_candidates(src_dir, "Second");
        assert_eq!(missing.len(), 0, "Second is not yet defined");

        // Simulate a long-lived rust-analyzer session adding a new file
        // between two top-level macro invocations.
        std::fs::write(
            src_dir.join("second.rs"),
            "pub struct Second { pub name: String }",
        )
        .unwrap();
        bump_epoch();

        let second = get_struct_candidates(src_dir, "Second");
        assert_eq!(
            second.len(),
            1,
            "newly added second.rs must appear after the directory fingerprint changes",
        );
        // First.rs must still be reachable — the rebuild does not lose
        // previously indexed structs.
        let first_again = get_struct_candidates(src_dir, "First");
        assert_eq!(first_again.len(), 1, "First must remain after rebuild");
    }

    /// Within a single epoch, repeated `get_struct_candidates` calls must
    /// not rewalk the directory. The first call walks + builds; subsequent
    /// calls in the same epoch reuse the cached `DirEntry` with no
    /// `fs::metadata` syscalls.
    #[serial_test::serial]
    #[test]
    fn test_file_list_skips_walk_within_same_epoch() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();
        std::fs::write(src_dir.join("a.rs"), "pub struct Alpha { pub id: i32 }").unwrap();
        std::fs::write(src_dir.join("b.rs"), "pub struct Beta { pub name: String }").unwrap();

        reset_metadata_call_count();
        bump_epoch();
        let before = metadata_call_count();

        let _ = get_struct_candidates(src_dir, "Alpha");
        let after_first = metadata_call_count();
        assert!(
            after_first > before,
            "first call must walk the directory (mtime syscalls expected)",
        );

        // Subsequent calls in the same epoch reuse the validated
        // `DirEntry` — zero new mtime syscalls for the file-list walk.
        let _ = get_struct_candidates(src_dir, "Beta");
        let _ = get_struct_candidates(src_dir, "Alpha");
        assert_eq!(
            metadata_call_count(),
            after_first,
            "same-epoch lookups must not rewalk the directory",
        );
    }

    /// Sanity check: the struct identifier index returns *every* file
    /// that defines a struct of the given name. Disambiguation by
    /// schema-name hint happens in
    /// [`super::file_lookup::find_struct_by_name_in_all_files`] *after*
    /// the candidate set is returned, so this layer must not pre-filter.
    #[serial_test::serial]
    #[test]
    fn test_struct_index_preserves_disambiguation_candidates() {
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

        bump_epoch();
        let candidates = get_struct_candidates(src_dir, "Model");
        assert_eq!(
            candidates.len(),
            2,
            "both files defining Model must be returned for the disambiguation layer",
        );
    }
}
