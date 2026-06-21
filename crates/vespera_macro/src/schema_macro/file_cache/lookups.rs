//! Phase-4 path-string resolution caches, split out of `file_cache.rs` to
//! keep that module within the project's source-size budget.
//!
//! These caches key on schema PATH strings (not file paths) and resolve
//! through the lower file-content / struct-definition mtime caches in the
//! parent [`super`] module. They form a conceptually distinct layer (path
//! resolution: struct / FK / module-path / circular lookups) from the raw
//! file/dir/content caching that remains in `file_cache.rs`, and operate on a
//! disjoint set of [`super::FileCache`] fields (`circular_analysis`,
//! `struct_lookup`, `fk_column_lookup`, `module_path_cache`). They share the
//! parent's `FILE_CACHE` thread-local plus the `ensure_file_list` /
//! `get_mtime_cached` helpers via `super::`.
//!
//! Pure code move out of `file_cache.rs` — no logic or behaviour change.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use crate::metadata::StructMetadata;
use crate::schema_macro::circular::{CircularAnalysis, analyze_circular_refs};
use crate::schema_macro::file_lookup::{
    find_fk_column_from_target_entity, find_struct_from_schema_path,
};

use super::{FILE_CACHE, FileCache, PathLookupEntry, ensure_file_list, get_mtime_cached};

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

    let result = analyze_circular_refs(source_module_path, definition);

    FILE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .circular_analysis
            .insert(key, result.clone());
    });

    result
}

/// Re-stamp the path-keyed lookup caches (`struct_lookup`, `fk_column_lookup`)
/// to the current epoch.
///
/// These caches **deliberately survive epoch bumps** (see the
/// `path_lookup_epoch` field): keeping resolved path lookups warm across
/// invocations lets repeated `schema_type!` / `#[derive(Schema)]` expansions in
/// one crate build share path-resolution work. They key on a schema PATH string
/// (not a file), so a cache MISS re-resolves through the lower file-content /
/// struct-definition mtime caches; within a single `cargo build` no source file
/// changes mid-build, so a surviving entry only ever returns the result a
/// re-resolution would produce. The epoch stamp is retained only for
/// cache-format / test compatibility.
///
/// (A long-lived rust-analyzer proc-macro server therefore keeps a resolved
/// entry until the server restarts — the accepted cost of the shared-work
/// optimisation. A future mtime-aware path cache could be both warm AND fresh,
/// but that is a design change, not a one-line tweak.)
fn path_lookup_fingerprint(cache: &mut FileCache, path_str: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path_str.hash(&mut hasher);

    let Some(manifest_dir) = get_manifest_dir_inner(cache) else {
        return hasher.finish();
    };
    let src_dir = Path::new(&manifest_dir).join("src");
    src_dir.hash(&mut hasher);

    let segments: Vec<&str> = path_str
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
        .collect();

    if segments.len() <= 1 {
        let files = ensure_file_list(cache, &src_dir);
        for path in files.iter() {
            fingerprint_path(cache, path, &mut hasher);
        }
        return hasher.finish();
    }

    let module_segments = &segments[..segments.len() - 1];
    let joined = module_segments.join("/");
    let candidates = [
        src_dir.join(format!("{joined}.rs")),
        src_dir.join(format!("{joined}/mod.rs")),
    ];
    for path in &candidates {
        fingerprint_path(cache, path, &mut hasher);
    }

    hasher.finish()
}

fn get_manifest_dir_inner(cache: &mut FileCache) -> Option<String> {
    let epoch = cache.epoch;
    if cache.manifest_dir_epoch == epoch
        && let Some(ref dir) = cache.manifest_dir
    {
        return Some(dir.clone());
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    cache.manifest_dir.clone_from(&dir);
    cache.manifest_dir_epoch = epoch;
    dir
}

fn fingerprint_path(cache: &mut FileCache, path: &Path, hasher: &mut DefaultHasher) {
    path.hash(hasher);
    match get_mtime_cached(cache, path) {
        Some(mtime) => {
            "mtime:some".hash(hasher);
            if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                duration.as_secs().hash(hasher);
                duration.subsec_nanos().hash(hasher);
            }
        }
        None => "mtime:none".hash(hasher),
    }
}

fn ensure_path_lookup_caches_fresh(cache: &mut FileCache) {
    cache.path_lookup_epoch = cache.epoch;
}

/// Get or compute struct lookup by schema path, with caching.
///
/// Wraps `find_struct_from_schema_path` with a
/// `HashMap<String, Option<Arc<StructMetadata>>>` cache. `None` values
/// are cached too (negative cache) to avoid repeated failed lookups.
/// The `Arc` makes cache hits O(1) instead of cloning the full struct
/// definition text per lookup.
///
/// The cache **survives epoch bumps** (see
/// [`ensure_path_lookup_caches_fresh`]): entries key on a schema PATH string,
/// and a cache MISS re-resolves through the lower file-content /
/// struct-definition mtime caches — so within one `cargo build` (no source
/// file changes mid-build) a surviving entry only ever returns the result a
/// re-resolution would produce, while keeping repeated lookups O(1). A
/// long-lived rust-analyzer proc-macro server therefore keeps a resolved
/// entry until the server restarts — the documented cost of the shared-work
/// optimisation (a future mtime-aware path cache could be warm AND fresh).
pub fn get_struct_from_schema_path(path_str: &str) -> Option<Arc<StructMetadata>> {
    // Re-stamp the path-lookup epoch (entries deliberately SURVIVE bumps — see
    // `ensure_path_lookup_caches_fresh`), then read the cache. The borrow ends
    // before the lookup below, which re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        ensure_path_lookup_caches_fresh(&mut cache);
        let fingerprint = path_lookup_fingerprint(&mut cache, path_str);
        cache.struct_lookup.get(path_str).and_then(|entry| {
            if entry.last_epoch_validated == cache.epoch || entry.fingerprint == fingerprint {
                Some(entry.value.clone())
            } else {
                None
            }
        })
    });
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().struct_lookup_cache_hits += 1);
        return result;
    }

    let result = find_struct_from_schema_path(path_str).map(Arc::new);

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let fingerprint = path_lookup_fingerprint(&mut cache, path_str);
        let epoch = cache.epoch;
        cache.struct_lookup.insert(
            path_str.to_string(),
            PathLookupEntry {
                value: result.clone(),
                fingerprint,
                last_epoch_validated: epoch,
            },
        );
    });

    result
}

/// Get or compute FK column lookup, with caching.
///
/// Wraps `find_fk_column_from_target_entity` with a `HashMap<(String, String), Option<String>>`
/// cache. Negative results (`None`) are cached to avoid repeated file lookups.
pub fn get_fk_column(schema_path: &str, via_rel: &str) -> Option<String> {
    let key = (schema_path.to_string(), via_rel.to_string());

    // Re-stamp the path-lookup epoch (entries deliberately SURVIVE bumps — see
    // `ensure_path_lookup_caches_fresh`), then read this epoch's cache. The
    // borrow ends before the lookup below, which re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        ensure_path_lookup_caches_fresh(&mut cache);
        let fingerprint = path_lookup_fingerprint(&mut cache, schema_path);
        cache.fk_column_lookup.get(&key).and_then(|entry| {
            if entry.last_epoch_validated == cache.epoch || entry.fingerprint == fingerprint {
                Some(entry.value.clone())
            } else {
                None
            }
        })
    });
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().fk_column_cache_hits += 1);
        return result;
    }

    let result = find_fk_column_from_target_entity(schema_path, via_rel);

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let fingerprint = path_lookup_fingerprint(&mut cache, schema_path);
        let epoch = cache.epoch;
        cache.fk_column_lookup.insert(
            key,
            PathLookupEntry {
                value: result.clone(),
                fingerprint,
                last_epoch_validated: epoch,
            },
        );
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
