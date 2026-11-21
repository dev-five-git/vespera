//! Phase-4 path-string resolution caches, split out of `file_cache.rs` to
//! keep that module within the project's source-size budget.
//!
//! These caches key on schema PATH strings (not file paths) and resolve
//! through the lower file-content / struct-definition mtime caches in the
//! parent [`super`] module. They form a conceptually distinct layer (path
//! resolution: struct / FK / module-path / circular lookups) from the raw
//! file/dir/content caching that remains in `file_cache.rs`, and operate on a
//! disjoint set of [`super::FileCache`] fields (`circular_analysis`,
//! `struct_lookup`, `fk_column_lookup`, `module_path_cache`;
//! `mtime_epoch_cache` is written only through `get_fingerprint_cached`, which
//! these lookups call to fingerprint the files they read). They share the
//! parent's `FILE_CACHE` thread-local plus the `ensure_file_list` /
//! `get_fingerprint_cached` helpers via `super::`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::metadata::StructMetadata;
use crate::schema_macro::circular::{CircularAnalysis, analyze_circular_refs};
use crate::schema_macro::file_lookup::{
    find_fk_column_from_target_entity, find_struct_from_schema_path,
};

use super::{
    FILE_CACHE, FileCache, PathLookupEntry, ensure_file_list, get_fingerprint_cached, tracked,
};

/// Outcome of probing a path-keyed lookup cache.
///
/// `Hit` short-circuits with the cached value. `Miss` carries the
/// `path_lookup_fingerprint` already computed during the probe, so the insert
/// path can reuse it instead of recomputing the (potentially `src`-tree-walking)
/// fingerprint a second time — the redundant double-fingerprint the previous
/// read-then-insert pair paid on every cache miss — plus the number of files
/// read so far: a resolution that reads a NEW file (a `#[path]` module
/// outside the module directory) recomputes it, so that file is covered.
enum Probe<T> {
    Hit(T),
    Miss(u64, usize),
}

/// The fingerprint a miss stores: the probe's, unless the resolution read a
/// file not read before (then that file may be one outside the module
/// directory the probe's fingerprint did not cover).
fn insert_fingerprint(
    cache: &mut FileCache,
    path_str: &str,
    (fingerprint, files_read): (u64, usize),
) -> u64 {
    if cache.mtime_epoch_cache.len() == files_read {
        fingerprint
    } else {
        path_lookup_fingerprint(cache, path_str)
    }
}

/// Every file this thread has probed that the `src_dir` `.rs` listing does
/// not cover: outside `src_dir` (`#[path = "../generated/models.rs"]`
/// modules the resolution opened, or looked for: a missing one is probed
/// too, and creating it later must invalidate the cached miss — only the
/// files READ used to be covered), or inside it without the `.rs` extension
/// (`#[path = "models.in"]`, L10 round 16), sorted.
fn out_of_tree_files(cache: &FileCache, src_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = cache
        .mtime_epoch_cache
        .keys()
        // `src/../generated/x.rs` (how a `#[path]` is joined) is outside too.
        .filter(|path| {
            !path.starts_with(src_dir)
                || path.extension().is_none_or(|ext| ext != "rs")
                || path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        })
        .cloned()
        .collect();
    files.sort();
    files
}

/// Get or compute circular reference analysis, with caching.
///
/// The cache key is `(source module | related module, definition)`; an
/// entry lives for one epoch (macro invocation), since a transitive cycle
/// check reads other models from disk.
pub fn get_circular_analysis(
    source_module_path: &[String],
    related_module_path: &[String],
    definition: &str,
) -> CircularAnalysis {
    let key = (
        format!(
            "{}|{}",
            source_module_path.join("::"),
            related_module_path.join("::")
        ),
        definition.to_string(),
    );

    // The borrow must end before analyzing: analysis re-enters FILE_CACHE.
    let cached = FILE_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .circular_analysis
            .get(&key)
            .filter(|(epoch, _)| *epoch == cache.epoch)
            .map(|(_, analysis)| analysis.clone())
    });
    if let Some(result) = cached {
        FILE_CACHE.with(|cache| cache.borrow_mut().circular_cache_hits += 1);
        return result;
    }

    let result = analyze_circular_refs(source_module_path, related_module_path, definition);

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = cache.epoch;
        cache.circular_analysis.insert(key, (epoch, result.clone()));
    });

    result
}

/// Dependency fingerprint for a schema PATH string, used to validate the
/// path-keyed lookup caches (`struct_lookup`, `fk_column_lookup`) across
/// epochs.
///
/// The digest folds in the path string, the crate's module directory, and
/// the mtime+len [`super::FileFingerprint`] of every file the resolution could
/// read: every `.rs` file of the crate (the module's own file and the
/// module tree from the crate root), the crate root files, and every file
/// read outside the module directory (`#[path]` modules). Comparing
/// it against the stored `PathLookupEntry::fingerprint` is what makes a
/// surviving entry safe — an entry whose inputs changed on disk fails the
/// comparison and is re-resolved.
///
/// Together with `PathLookupEntry::last_epoch_validated` (which only skips
/// recomputing this fingerprint for an entry already validated in the
/// current epoch) this is the sole freshness mechanism for those caches: they
/// stay warm across epoch bumps so repeated `schema_type!` /
/// `#[derive(Schema)]` expansions in one crate build share path-resolution
/// work, and stay correct because a disk change perturbs the fingerprint.
fn path_lookup_fingerprint(cache: &mut FileCache, path_str: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path_str.hash(&mut hasher);

    let Some(manifest_dir) = super::get_manifest_dir_inner(cache) else {
        return hasher.finish();
    };
    let layout = crate::schema_macro::crate_layout::layout_for(
        Path::new(&manifest_dir),
        proc_macro2::Span::call_site().local_file().as_deref(),
    );
    crate_fingerprint(cache, layout).hash(&mut hasher);
    hasher.finish()
}

/// The crate-wide part of [`path_lookup_fingerprint`], memoised per epoch,
/// layout and number of files read so far (`out_of_tree_files` grows as the
/// invocation reads `#[path]` files): hashing every source file once per
/// distinct lookup key made each `schema_type!` O(keys x files).
fn crate_fingerprint(
    cache: &mut FileCache,
    layout: crate::schema_macro::crate_layout::CrateLayout,
) -> u64 {
    // The memo keeps the inputs of its computation (R24-SM1-L3): a hit must
    // report the listing and files the fingerprint covers to the enclosing
    // tracked computation, as the miss did.
    type Memo = Option<(
        (u64, usize),
        crate::schema_macro::crate_layout::CrateLayout,
        u64,
        tracked::Deps,
    )>;
    thread_local! {
        static MEMO: std::cell::RefCell<Memo> = const { std::cell::RefCell::new(None) };
    }
    let at = (cache.epoch, cache.mtime_epoch_cache.len());
    let hit = MEMO.with(|memo| {
        memo.borrow()
            .as_ref()
            .filter(|(memo_at, memo_layout, _, _)| *memo_at == at && *memo_layout == layout)
            .map(|(_, _, fingerprint, deps)| (*fingerprint, deps.clone()))
    });
    if let Some((fingerprint, deps)) = hit {
        tracked::replay(&deps);
        return fingerprint;
    }
    let (fingerprint, deps) = tracked::tracked(|| {
        let mut hasher = DefaultHasher::new();
        let src_dir = &layout.module_dir;
        src_dir.hash(&mut hasher);

        // Every source file of the crate: a qualified path is resolved through
        // its module's own file AND, failing that, the module tree from the
        // crate root (`mod` declarations, `#[path]`, inline modules), so an
        // edit to any of those files can change the answer (the module's two
        // candidate files alone left a long-lived rust-analyzer stale).
        let files = ensure_file_list(cache, src_dir);
        let outside = out_of_tree_files(cache, src_dir);
        for path in files.iter().chain(&layout.roots).chain(&outside) {
            fingerprint_path(cache, path, &mut hasher);
        }
        hasher.finish()
    });
    // Keyed by the probe count AFTER hashing: the first hash of an epoch
    // probes the source files itself.
    let at = (cache.epoch, cache.mtime_epoch_cache.len());
    MEMO.with(|memo| *memo.borrow_mut() = Some((at, layout, fingerprint, deps)));
    fingerprint
}

fn fingerprint_path(cache: &mut FileCache, path: &Path, hasher: &mut DefaultHasher) {
    path.hash(hasher);
    match get_fingerprint_cached(cache, path) {
        Some(fp) => {
            "fp:some".hash(hasher);
            if let Ok(duration) = fp.mtime.duration_since(std::time::UNIX_EPOCH) {
                duration.as_secs().hash(hasher);
                duration.subsec_nanos().hash(hasher);
            }
            // Length folds in alongside mtime so a size-changing,
            // timestamp-preserving edit re-resolves the path lookup.
            fp.len.hash(hasher);
        }
        None => "fp:none".hash(hasher),
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
/// The cache **survives epoch bumps**; freshness comes from
/// `PathLookupEntry::last_epoch_validated` plus the recomputed
/// [`path_lookup_fingerprint`]. An entry already validated in the current
/// epoch is trusted as-is; otherwise the fingerprint is recomputed and the
/// entry is only reused when it still matches, so a source edit that changes
/// any file the resolution reads forces a re-resolution — including in a
/// long-lived rust-analyzer proc-macro server.
pub fn get_struct_from_schema_path(path_str: &str) -> Option<Arc<StructMetadata>> {
    // Probe the cache; the borrow ends before the lookup below, which
    // re-enters FILE_CACHE. The probe is tracked: its own reads (the
    // fingerprint) and a served entry's inputs both reach the caller's frame.
    let (probe, mut deps) = tracked::tracked(|| {
        FILE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let epoch = cache.epoch;
            // Epoch-hit fast path: an entry already validated THIS epoch is fresh
            // WITHOUT recomputing the fingerprint — which, for a single-segment
            // path, walks the entire `src` tree. The previous code computed the
            // fingerprint unconditionally even when the epoch stamp already proved
            // freshness.
            if let Some(entry) = cache.struct_lookup.get(path_str)
                && entry.last_epoch_validated == epoch
            {
                tracked::replay(&entry.deps);
                return Probe::Hit(entry.value.clone());
            }
            // Cross-epoch / absent: compute the fingerprint ONCE and reuse it for
            // the insert below on a miss.
            let fingerprint = path_lookup_fingerprint(&mut cache, path_str);
            if let Some(entry) = cache.struct_lookup.get_mut(path_str)
                && entry.fingerprint == fingerprint
            {
                // Re-stamp so further lookups this epoch take the fast path.
                entry.last_epoch_validated = epoch;
                tracked::replay(&entry.deps);
                return Probe::Hit(entry.value.clone());
            }
            Probe::Miss(fingerprint, cache.mtime_epoch_cache.len())
        })
    });
    let fingerprint = match probe {
        Probe::Hit(value) => {
            FILE_CACHE.with(|cache| cache.borrow_mut().struct_lookup_cache_hits += 1);
            return value;
        }
        Probe::Miss(fingerprint, files_read) => (fingerprint, files_read),
    };

    let (result, resolved_deps) =
        tracked::tracked(|| find_struct_from_schema_path(path_str).map(Arc::new));
    deps.extend(&resolved_deps);

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = cache.epoch;
        let fingerprint = insert_fingerprint(&mut cache, path_str, fingerprint);
        cache.struct_lookup.insert(
            path_str.to_string(),
            PathLookupEntry {
                value: result.clone(),
                fingerprint,
                last_epoch_validated: epoch,
                deps,
            },
        );
    });

    result
}

/// Get or compute FK column lookup, with caching.
///
/// Wraps `find_fk_column_from_target_entity` with a
/// `HashMap<(String, String), PathLookupEntry<Option<FkColumns>>>` cache
/// (`FkColumns` = `(from, Option<to>)`). Negative results (`None`) are
/// cached to avoid repeated file lookups.
pub fn get_fk_column(schema_path: &str, via_rel: &str) -> Option<(String, Option<String>)> {
    let key = (schema_path.to_string(), via_rel.to_string());

    // Probe the cache (entries deliberately SURVIVE epoch bumps — freshness
    // comes from `last_epoch_validated` + `path_lookup_fingerprint`). The
    // borrow ends before the lookup below, which re-enters FILE_CACHE. The
    // probe is tracked, as in `get_struct_from_schema_path` (R23-M1).
    let (probe, mut deps) = tracked::tracked(|| {
        FILE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let epoch = cache.epoch;
            // Epoch-hit fast path: skip the (possibly `src`-tree-walking)
            // fingerprint when the entry was already validated this epoch.
            if let Some(entry) = cache.fk_column_lookup.get(&key)
                && entry.last_epoch_validated == epoch
            {
                tracked::replay(&entry.deps);
                return Probe::Hit(entry.value.clone());
            }
            // Cross-epoch / absent: compute the fingerprint ONCE, reuse on miss.
            let fingerprint = path_lookup_fingerprint(&mut cache, schema_path);
            if let Some(entry) = cache.fk_column_lookup.get_mut(&key)
                && entry.fingerprint == fingerprint
            {
                entry.last_epoch_validated = epoch;
                tracked::replay(&entry.deps);
                return Probe::Hit(entry.value.clone());
            }
            Probe::Miss(fingerprint, cache.mtime_epoch_cache.len())
        })
    });
    let fingerprint = match probe {
        Probe::Hit(value) => {
            FILE_CACHE.with(|cache| cache.borrow_mut().fk_column_cache_hits += 1);
            return value;
        }
        Probe::Miss(fingerprint, files_read) => (fingerprint, files_read),
    };

    let (result, resolved_deps) =
        tracked::tracked(|| find_fk_column_from_target_entity(schema_path, via_rel));
    deps.extend(&resolved_deps);

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = cache.epoch;
        let fingerprint = insert_fingerprint(&mut cache, schema_path, fingerprint);
        cache.fk_column_lookup.insert(
            key,
            PathLookupEntry {
                value: result.clone(),
                fingerprint,
                last_epoch_validated: epoch,
                deps,
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

    // Raw identifiers (`r#type`) are un-raw'd like every other module path.
    let mut result: Vec<String> = path_str
        .split("::")
        .map(|s| s.trim().trim_start_matches("r#"))
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::schema_macro::file_cache::bump_epoch;

    /// Without a manifest dir the fingerprint is the path string's hash alone.
    #[test]
    fn fingerprint_without_manifest_uses_path_only() {
        let _env_guard = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        bump_epoch();

        let path_only = |path: &str| {
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            hasher.finish()
        };
        FILE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            assert_eq!(
                path_lookup_fingerprint(&mut cache, "crate::Model"),
                path_only("crate::Model")
            );
            assert_ne!(
                path_lookup_fingerprint(&mut cache, "crate::Other"),
                path_only("crate::Model")
            );
        });
    }

    /// Unchanged sources: the struct and FK lookups resolve (the fixture's FK
    /// relation is a real `Model` field) and the next epoch serves both from
    /// the cache — the hit counters grow, the struct `Arc` is shared.
    #[test]
    fn struct_and_fk_lookups_revalidate_unchanged_entries_across_epochs() {
        let temp = TempDir::new().unwrap();
        let model_dir = temp.path().join("src/models");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(
            model_dir.join("user.rs"),
            r#"pub struct Model {
                   pub owner_id: i32,
                   #[sea_orm(belongs_to, relation_enum = "Owner", from = "owner_id", to = "id")]
                   pub owner: BelongsTo<super::owner::Entity>,
               }"#,
        )
        .unwrap();
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
        let hits = || {
            FILE_CACHE.with(|cache| {
                let cache = cache.borrow();
                (cache.struct_lookup_cache_hits, cache.fk_column_cache_hits)
            })
        };

        bump_epoch();
        let first = get_struct_from_schema_path("crate::models::user::Model")
            .expect("fixture model resolves");
        assert!(first.definition.contains("owner_id"));
        let first_fk = get_fk_column("crate::models::user::Schema", "Owner");
        assert_eq!(
            first_fk,
            Some(("owner_id".to_string(), Some("id".to_string())))
        );
        let (struct_hits, fk_hits) = hits();

        bump_epoch();
        let second = get_struct_from_schema_path("crate::models::user::Model")
            .expect("unchanged fixture remains cached");
        let second_fk = get_fk_column("crate::models::user::Schema", "Owner");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first_fk, second_fk);
        assert_eq!(hits(), (struct_hits + 1, fk_hits + 1));
    }

    /// R23-M1: a path lookup served from the epoch fast path inside a `tracked`
    /// computation reports the files it depends on, so a memo built on it goes
    /// stale when the source changes. The fast path used to record nothing, so
    /// the memo kept its stale value after the model file was rewritten.
    #[test]
    fn epoch_hit_inside_tracked_computation_reports_the_lookup_inputs() {
        use crate::schema_macro::file_cache::Tracked;
        let temp = TempDir::new().unwrap();
        let model_dir = temp.path().join("src/models");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_file = model_dir.join("user.rs");
        std::fs::write(&model_file, "pub struct Model { pub id: i32 }").unwrap();
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
        let path = "crate::models::user::Model";

        bump_epoch();
        // Validates the entry in this epoch, so the next lookup takes the fast path.
        assert!(get_struct_from_schema_path(path).is_some());
        // Same epoch, inside a tracked computation: served by the fast path.
        let mut memo = Tracked::compute(|| get_struct_from_schema_path(path).is_some());
        assert_eq!(memo.get(), Some(true));

        std::fs::write(&model_file, "pub struct Renamed { pub id: i32 }").unwrap();
        bump_epoch();
        assert_eq!(memo.get(), None, "the memo must notice the rewritten model");
    }

    /// A `#[path]` module outside the module directory is part of the
    /// fingerprint: editing it re-resolves the cached lookup.
    #[test]
    fn out_of_tree_path_module_edit_invalidates_the_lookup() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("src");
        let generated = temp.path().join("generated");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[path = \"../generated/models.rs\"]\npub mod models;",
        )
        .unwrap();
        let models = generated.join("models.rs");
        std::fs::write(&models, "pub struct Model { pub a: i32 }").unwrap();
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());

        bump_epoch();
        let first = get_struct_from_schema_path("crate::models::Model").expect("resolves");
        assert!(first.definition.contains("pub a"));

        std::fs::write(&models, "pub struct Model { pub renamed_field: i64 }").unwrap();
        bump_epoch();
        let second = get_struct_from_schema_path("crate::models::Model").expect("resolves");
        assert!(
            second.definition.contains("renamed_field"),
            "{}",
            second.definition
        );
    }

    /// L10 (round 16): a `#[path]` module inside the module directory without
    /// the `.rs` extension (missing from the `.rs` listing) is fingerprinted
    /// too: editing it re-resolves the cached lookup (it stayed stale).
    #[test]
    fn non_rs_path_module_edit_invalidates_the_lookup() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[path = \"models.in\"]\npub mod models;",
        )
        .unwrap();
        let models = src.join("models.in");
        std::fs::write(&models, "pub struct Model { pub a: i32 }").unwrap();
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());

        bump_epoch();
        let first = get_struct_from_schema_path("crate::models::Model").expect("resolves");
        assert!(first.definition.contains("pub a"));

        std::fs::write(&models, "pub struct Model { pub renamed_field: i64 }").unwrap();
        bump_epoch();
        let second = get_struct_from_schema_path("crate::models::Model").expect("resolves");
        assert!(
            second.definition.contains("renamed_field"),
            "{}",
            second.definition
        );
    }

    /// The second lookup of one schema path is a cache hit.
    #[test]
    fn module_path_lookup_returns_cached_value() {
        let schema_path = quote::quote!(crate::models::user::Schema);
        let expected = vec!["crate", "models", "user"];
        let hits = || FILE_CACHE.with(|cache| cache.borrow().module_path_cache_hits);
        assert_eq!(get_module_path_from_schema_path(&schema_path), expected);
        let before = hits();
        assert_eq!(get_module_path_from_schema_path(&schema_path), expected);
        assert_eq!(hits(), before + 1);
    }

    /// L4 (round 13): a probed but MISSING file outside the module directory
    /// (a `#[path = "../generated/models.rs"]` not generated yet) is part of
    /// the fingerprint, so creating it invalidates a cached miss; only the files
    /// actually read used to be covered.  (L8, round 18: the test only checked
    /// the probe was recorded; it now creates the file and re-resolves.)
    #[test]
    fn missing_out_of_tree_probe_is_fingerprinted() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[path = \"../generated/models.rs\"]\npub mod models;",
        )
        .unwrap();
        let missing = temp.path().join("generated").join("models.rs");
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
        bump_epoch();
        assert!(get_struct_from_schema_path("crate::models::Model").is_none());
        FILE_CACHE.with(|cache| {
            let probes = out_of_tree_files(&cache.borrow(), &src);
            assert!(
                probes
                    .iter()
                    .any(|probe| probe.ends_with("generated/models.rs")),
                "{probes:?}"
            );
        });
        std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
        std::fs::write(&missing, "pub struct Model { pub generated: i32 }").unwrap();
        bump_epoch();
        let found = get_struct_from_schema_path("crate::models::Model")
            .expect("creating the probed file invalidates the cached miss");
        assert!(
            found.definition.contains("generated"),
            "{}",
            found.definition
        );
    }

    /// L5 (round 13): circular analyses are readable only in their epoch, so
    /// the epoch bump drops them (they used to pile up in a long-lived host).
    #[test]
    fn circular_analysis_is_evicted_on_epoch_bump() {
        bump_epoch();
        let _ = get_circular_analysis(
            &["crate".to_string()],
            &["crate".to_string(), "x".to_string()],
            "pub struct Model { pub id: i32 }",
        );
        FILE_CACHE.with(|cache| assert!(!cache.borrow().circular_analysis.is_empty()));
        bump_epoch();
        FILE_CACHE.with(|cache| assert!(cache.borrow().circular_analysis.is_empty()));
    }

    /// R24-SM1-L3: a crate fingerprint served from the memo still reports the
    /// crate listing to the enclosing tracked computation. The memo hit used to
    /// return before the listing was recorded, so a file created in `src/` after
    /// the computation left its dependencies looking valid.
    #[test]
    fn crate_fingerprint_memo_hit_reports_the_crate_listing() {
        use crate::schema_macro::file_cache::tracked;
        let temp = TempDir::new().unwrap();
        let models = temp.path().join("src/models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub mod models;").unwrap();
        std::fs::write(models.join("mod.rs"), "pub mod user;").unwrap();
        std::fs::write(models.join("user.rs"), "pub struct Model { pub id: i32 }").unwrap();
        let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());

        bump_epoch();
        let layout = crate::schema_macro::crate_layout::layout_for(temp.path(), None);
        let fingerprint = |layout: &crate::schema_macro::crate_layout::CrateLayout| {
            FILE_CACHE.with(|cache| crate_fingerprint(&mut cache.borrow_mut(), layout.clone()))
        };
        // Outside any tracked computation: computes and memoizes the fingerprint.
        let first = fingerprint(&layout);
        // Same epoch, same probes: the memo answers, and its inputs are reported.
        let (second, deps) = tracked::tracked(|| fingerprint(&layout));
        assert_eq!(first, second);
        assert!(deps.still_valid());

        std::fs::write(temp.path().join("src/extra.rs"), "").unwrap();
        // Fingerprints are read once per epoch: the next epoch sees the new file.
        bump_epoch();
        assert!(
            !deps.still_valid(),
            "a file created in src/ must invalidate the memo-hit computation"
        );
    }
}
