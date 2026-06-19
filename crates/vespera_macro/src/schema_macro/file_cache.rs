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
//! The epoch mechanism amortises this: each file-cache-reaching top-level macro
//! invocation (`#[derive(Schema)]`, `schema!`, `schema_type!`, `vespera!`, and
//! `export_app!`) calls [`bump_epoch`] once at entry. Within that epoch, a given
//! path's mtime is fetched from `fs::metadata` **at most once** and stored in
//! `mtime_epoch_cache`. Subsequent lookups for the same path in the same epoch
//! reuse the cached mtime without a syscall. `#[route]`, `#[cron]`, and
//! `#[derive(Multipart)]` do not call into this module: they parse only the
//! annotated item tokens and update in-memory macro storage, so they are
//! intentionally exempt.
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

// Test-only thread-local counter: number of `extract_struct_names`
// tokenisation passes (the per-file source scan). Lets the H1 regression
// benchmark prove that a single-file edit re-tokenises only the changed
// file instead of every file in the directory.
#[cfg(test)]
thread_local! {
    static EXTRACT_STRUCT_NAMES_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reset the test-only `extract_struct_names` call counter for this thread.
#[cfg(test)]
pub fn reset_extract_struct_names_count() {
    EXTRACT_STRUCT_NAMES_COUNT.with(|c| c.set(0));
}

/// Current value of the test-only `extract_struct_names` call counter.
#[cfg(test)]
pub fn extract_struct_names_count() -> usize {
    EXTRACT_STRUCT_NAMES_COUNT.with(std::cell::Cell::get)
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

    /// Per-file mtime-validated cache of the struct names defined in each
    /// `.rs` file (the [`extract_struct_names`] tokenisation result).
    ///
    /// The `struct_index` above is dropped wholesale whenever a directory's
    /// fingerprint changes (any file added / removed / modified — the common
    /// rust-analyzer edit). Without this per-file layer the rebuild
    /// re-tokenised **every** file in the directory; with it, a file whose
    /// mtime is unchanged returns its cached names in O(1), so only the
    /// genuinely changed file pays the O(file_size) tokenisation. The index
    /// rebuild then costs one tokenisation per *edited* file instead of one
    /// per file in the directory.
    file_struct_names: HashMap<PathBuf, (SystemTime, Arc<[String]>)>,

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
    /// Cached `CARGO_MANIFEST_DIR` value to avoid repeated `std::env::var`
    /// reads.  Constant within one compilation, but revalidated once per
    /// epoch (see [`get_manifest_dir`]) so a long-lived rust-analyzer
    /// proc-macro server reused across crates picks up the new manifest dir
    /// instead of resolving paths against the previous crate forever.
    manifest_dir: Option<String>,
    /// Epoch [`FileCache::manifest_dir`] was last read in (for the per-epoch
    /// revalidation above).
    manifest_dir_epoch: u64,

    // --- Phase 4 profiling counters ---
    circular_cache_hits: usize,
    struct_lookup_cache_hits: usize,
    fk_column_cache_hits: usize,
    module_path_cache_hits: usize,
    struct_def_cache_hits: usize,

    // --- Epoch caching ---
    /// Monotonically increasing counter. Bumped once at the start of each
    /// file-cache-reaching top-level macro invocation (`#[derive(Schema)]`,
    /// `schema!`, `schema_type!`, `vespera!`, `export_app!`).
    epoch: u64,
    /// Epoch the path-keyed lookup caches (`struct_lookup`,
    /// `fk_column_lookup`) were last populated for.
    ///
    /// Those two caches key on a schema PATH string rather than a file, so
    /// — unlike `file_contents` / `struct_definitions` — they cannot be
    /// mtime-validated per entry. Scoping them to one epoch drops stale
    /// entries when a model file is edited between macro invocations in a
    /// long-lived rust-analyzer proc-macro server.
    path_lookup_epoch: u64,
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
        file_struct_names: HashMap::with_capacity(32),
        file_disk_reads: 0,
        content_cache_hits: 0,
        struct_parses: 0,
        ast_parses: 0,
        circular_analysis: HashMap::with_capacity(16),
        struct_lookup: HashMap::with_capacity(32),
        fk_column_lookup: HashMap::with_capacity(16),
        module_path_cache: HashMap::with_capacity(32),
        manifest_dir: None,
        manifest_dir_epoch: 0,
        circular_cache_hits: 0,
        struct_lookup_cache_hits: 0,
        fk_column_cache_hits: 0,
        module_path_cache_hits: 0,
        struct_definitions: HashMap::with_capacity(32),
        struct_def_cache_hits: 0,
        epoch: 0,
        path_lookup_epoch: 0,
        mtime_epoch_cache: HashMap::with_capacity(32),
    });
}

/// Advance the per-invocation epoch counter.
///
/// Call this **once** at the start of each file-cache-reaching top-level macro
/// invocation (`#[derive(Schema)]`, `schema!`, `schema_type!`, `vespera!`,
/// `export_app!`). `#[route]`, `#[cron]`, and `#[derive(Multipart)]` are exempt
/// because they do not read files through this module. Within a single epoch,
/// `fs::metadata` is called at most once per path; subsequent lookups for the
/// same path reuse the cached mtime without a syscall.
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
/// Constant within one compilation, so the value is cached and reused for
/// the rest of the epoch — avoiding the 20+ `std::env::var` reads a single
/// `schema_type!` expansion would otherwise make. It is revalidated **once
/// per epoch**, though: a long-lived rust-analyzer proc-macro server can
/// reuse this thread to expand a DIFFERENT crate whose `CARGO_MANIFEST_DIR`
/// differs, and a stale value would resolve every cross-file lookup against
/// the previous crate's `src/`.
pub fn get_manifest_dir() -> Option<String> {
    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = cache.epoch;
        // Trust the cached value only within the epoch it was read in.
        if cache.manifest_dir_epoch == epoch
            && let Some(ref dir) = cache.manifest_dir
        {
            return Some(dir.clone());
        }
        let dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        cache.manifest_dir.clone_from(&dir);
        cache.manifest_dir_epoch = epoch;
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

    if let Some(entry) = cache.file_lists.get_mut(src_dir) {
        if entry.fingerprint == fp {
            // Unchanged directory: refresh the validation epoch IN PLACE and
            // hand back a single `Arc::clone`.  The previous code rebuilt the
            // whole `DirEntry` (a `to_path_buf` key allocation) and cloned the
            // `Arc` twice — once for the cache, once to return.
            entry.last_epoch_validated = current_epoch;
            return Arc::clone(&entry.files);
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
    #[cfg(test)]
    EXTRACT_STRUCT_NAMES_COUNT.with(|c| c.set(c.get() + 1));
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

/// Struct names defined in `path`, served from a per-file mtime-validated
/// cache so the directory struct-index rebuild re-tokenises only files whose
/// mtime actually changed.
///
/// On an mtime match the cached `Arc<[String]>` is cloned (O(1), no source
/// scan); otherwise the file content is read (via the mtime-validated content
/// cache) and re-tokenised once, then cached. A file that cannot be read
/// yields an empty name list — the caller simply contributes no candidates
/// for it, matching the prior inline `continue`-on-read-miss behaviour.
fn get_file_struct_names(cache: &mut FileCache, path: &Path) -> Arc<[String]> {
    let current_mtime = get_mtime_cached(cache, path);

    if let Some(mtime) = current_mtime
        && let Some((cached_mtime, names)) = cache.file_struct_names.get(path)
        && *cached_mtime == mtime
    {
        return Arc::clone(names);
    }

    let names: Arc<[String]> = get_file_content_inner(cache, path).map_or_else(
        || Vec::new().into(),
        |content| extract_struct_names(&content).into(),
    );

    if let Some(mtime) = current_mtime {
        cache
            .file_struct_names
            .insert(path.to_path_buf(), (mtime, Arc::clone(&names)));
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
                // Per-file mtime-validated names: unchanged files return their
                // cached tokenisation (O(1)); only an added/modified file pays
                // the source scan, so this rebuild costs one tokenisation per
                // *edited* file instead of one per file in the directory.
                for name in get_file_struct_names(&mut cache, path).iter() {
                    grouped.entry(name.clone()).or_default().push(path.clone());
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

/// Drop the path-keyed lookup caches (`struct_lookup`, `fk_column_lookup`)
/// when the epoch has advanced since they were last populated.
///
/// Unlike `file_contents` / `struct_definitions`, these caches key on a
/// schema PATH string rather than a file, so they cannot be mtime-validated
/// per entry. Without this, a long-lived rust-analyzer proc-macro server
/// would keep a stale `StructMetadata` (or stale FK column / negative
/// result) after a model file edit, indefinitely. Scoping them to a single
/// epoch (one top-level macro invocation) preserves the intra-invocation
/// caching — where the same path is resolved repeatedly for circular
/// references — while re-resolving across invocations through the lower
/// mtime-validated layers, so an edited file is always picked up.
fn ensure_path_lookup_caches_fresh(cache: &mut FileCache) {
    if cache.path_lookup_epoch != cache.epoch {
        cache.struct_lookup.clear();
        cache.fk_column_lookup.clear();
        cache.path_lookup_epoch = cache.epoch;
    }
}

/// Get or compute struct lookup by schema path, with caching.
///
/// Wraps `find_struct_from_schema_path` with a
/// `HashMap<String, Option<Arc<StructMetadata>>>` cache. `None` values
/// are cached too (negative cache) to avoid repeated failed lookups.
/// The `Arc` makes cache hits O(1) instead of cloning the full struct
/// definition text per lookup.
///
/// The cache is scoped to the current epoch
/// ([`ensure_path_lookup_caches_fresh`]): a new top-level macro invocation
/// drops prior entries so an edited model file is re-resolved (correctness
/// for long-lived rust-analyzer servers), while repeated lookups within the
/// same invocation still hit the cache.
pub fn get_struct_from_schema_path(path_str: &str) -> Option<Arc<StructMetadata>> {
    // Drop stale (pre-edit) entries when the epoch advanced, then read this
    // epoch's cache. The borrow ends before the lookup below, which
    // re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        ensure_path_lookup_caches_fresh(&mut cache);
        cache.struct_lookup.get(path_str).cloned()
    });
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

    // Drop stale entries when the epoch advanced, then read this epoch's
    // cache. The borrow ends before the lookup below, which re-enters
    // FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        ensure_path_lookup_caches_fresh(&mut cache);
        cache.fk_column_lookup.get(&key).cloned()
    });
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

/// Test-only: whether the FK-column lookup cache currently holds an entry
/// for `(schema_path, via_rel)`. Used to assert epoch-scoped invalidation.
#[cfg(test)]
pub fn fk_lookup_contains(schema_path: &str, via_rel: &str) -> bool {
    FILE_CACHE.with(|cache| {
        cache
            .borrow()
            .fk_column_lookup
            .contains_key(&(schema_path.to_string(), via_rel.to_string()))
    })
}

#[cfg(test)]
mod tests;
