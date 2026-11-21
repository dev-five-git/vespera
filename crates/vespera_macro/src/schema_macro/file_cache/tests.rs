use tempfile::TempDir;

use super::*;

/// The second identical lookup is served from the cache (the hit counter
/// grows), returning the first result.
#[test]
fn test_get_fk_column_cache_hit() {
    let hits = || FILE_CACHE.with(|cache| cache.borrow().fk_column_cache_hits);
    let result1 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
    let before = hits();
    let result2 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
    assert_eq!(result1, result2);
    assert_eq!(hits(), before + 1, "the second lookup must hit the cache");
}

/// Path-keyed lookup caches survive epoch bumps so repeated `schema_type!`
/// expansions in one crate share path resolution work. Staleness is guarded
/// by `path_lookup_fingerprint` (the entry is served again only while it
/// matches): the next epoch's identical lookup is a cache HIT.
#[test]
fn path_lookup_caches_survive_epoch_bumps() {
    let hits = || FILE_CACHE.with(|cache| cache.borrow().fk_column_cache_hits);
    // Fresh epoch; cache a (negative) FK result for this epoch.
    bump_epoch();
    let _ = get_fk_column("ra::stale::Schema", "Rel");
    assert!(
        fk_lookup_contains("ra::stale::Schema", "Rel"),
        "result must be cached within the same epoch"
    );
    // A second access in the SAME epoch keeps the cache populated.
    let _ = get_fk_column("ra::stale::Schema", "Rel");
    assert!(fk_lookup_contains("ra::stale::Schema", "Rel"));
    // Advancing the epoch (the next macro invocation) must not drop the
    // path-keyed caches anymore.
    bump_epoch();
    let _ = get_fk_column("ra::trigger::Schema", "Rel");
    assert!(
        fk_lookup_contains("ra::stale::Schema", "Rel"),
        "lookup entry must remain cached when the epoch advances"
    );
    // ...and it is served from the cache, not recomputed.
    let before = hits();
    let _ = get_fk_column("ra::stale::Schema", "Rel");
    assert_eq!(hits(), before + 1, "the next epoch's lookup must hit");
}

#[test]
fn path_lookup_revalidates_when_resolved_file_mtime_changes() {
    let temp_dir = TempDir::new().unwrap();
    let models_dir = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    let model_path = models_dir.join("user.rs");
    std::fs::write(&model_path, "pub struct Model { pub id: i32 }").unwrap();

    let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());

    bump_epoch();
    let first = get_struct_from_schema_path("crate::models::user::Model")
        .expect("initial model should resolve");
    assert!(first.definition.contains("id : i32"));

    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&model_path, "pub struct Model { pub name: String }").unwrap();

    bump_epoch();
    let second = get_struct_from_schema_path("crate::models::user::Model")
        .expect("edited model should resolve");
    assert!(
        second.definition.contains("name : String"),
        "path lookup must invalidate stale resolved-file entries after mtime changes: {}",
        second.definition
    );
}

#[test]
fn test_print_profile_summary_with_profile_env() {
    let _env_guard = crate::env_access::test_env::set_var("VESPERA_PROFILE", "1");

    // R36-SM1-L1: the report is asserted on, not just printed. Every counter
    // line is there, and a parse moves the `struct parses` line.
    let before = profile_summary_report().expect("VESPERA_PROFILE is set");
    assert!(before.starts_with("[vespera-profile] File cache stats:\n"));
    assert!(before.contains("\n  struct parses: "));
    assert!(before.contains("\n  module path: "));
    parse_struct_cached("pub struct Probe;").expect("probe parses");
    let after = profile_summary_report().expect("VESPERA_PROFILE is set");
    assert_ne!(before, after, "a struct parse must change the report");
    assert!(after.contains("\n  struct parses: "));

    let _env_guard = crate::env_access::test_env::remove_var("VESPERA_PROFILE");
}

#[test]
fn test_print_profile_summary_without_profile_env() {
    let _env_guard = crate::env_access::test_env::remove_var("VESPERA_PROFILE");

    assert!(profile_summary_report().is_none());
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

#[test]
fn test_missing_file_content_is_negative_cached_within_epoch() {
    let temp_dir = TempDir::new().unwrap();
    let missing_path = temp_dir.path().join("missing.rs");

    reset_metadata_call_count();
    bump_epoch();
    let before = metadata_call_count();
    let reads = read_attempt_count();

    assert!(get_struct_definition(&missing_path, "Missing").is_none());
    assert!(get_struct_definition(&missing_path, "Missing").is_none());

    assert_eq!(
        metadata_call_count() - before,
        1,
        "missing file should be stat'd once in one epoch"
    );
    // The negative cache itself: one read attempt, not one per lookup (the
    // stat count above is 1 through the mtime cache alone).
    assert_eq!(
        read_attempt_count() - reads,
        1,
        "missing file should be read once in one epoch"
    );
}

/// Verify cross-entry invalidation semantics.
///
/// In a long-lived rust-analyzer proc-macro server the same thread handles
/// multiple successive macro invocations.  Each entry point (`derive_schema`,
/// `schema_type!`, `schema!`, `export_app!`, `vespera!`, `#[route]`,
/// `#[cron]`) calls `bump_epoch()` before its first file-cache read.  This test simulates two successive invocations
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
///   - `#[route]`       → reaches file_cache via the Path-extractor check,
///     the return-alias expansion and inline_module_of (route_impl.rs)
///   - `#[cron]`        → reaches file_cache via inline_module_of (cron_impl.rs)
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

/// Within a single epoch, repeated single-segment path lookups must not
/// rewalk the `src` tree. `path_lookup_fingerprint` folds every `.rs` file
/// under `src` into the digest via `ensure_file_list`, so the first lookup
/// walks + stats; subsequent lookups in the same epoch reuse the validated
/// `DirEntry` (and the per-epoch fingerprint cache) with zero new
/// `fs::metadata` syscalls.
#[test]
fn test_file_list_skips_walk_within_same_epoch() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.rs"), "pub struct Alpha { pub id: i32 }").unwrap();
    std::fs::write(src_dir.join("b.rs"), "pub struct Beta { pub name: String }").unwrap();

    let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());

    reset_metadata_call_count();
    bump_epoch();
    let before = metadata_call_count();

    let _ = get_struct_from_schema_path("Alpha");
    let after_first = metadata_call_count();
    assert!(
        after_first > before,
        "first lookup must walk the directory (mtime syscalls expected)",
    );

    // Subsequent lookups in the same epoch reuse the validated
    // `DirEntry` — zero new mtime syscalls for the file-list walk.
    let _ = get_struct_from_schema_path("Beta");
    let _ = get_struct_from_schema_path("Alpha");
    assert_eq!(
        metadata_call_count(),
        after_first,
        "same-epoch lookups must not rewalk the directory",
    );
}

/// L24: a qualified path resolved through the module tree (an inline module
/// of the crate root) is re-resolved when the crate root changes; the
/// fingerprint used to cover only the module's own candidate files.
#[test]
fn qualified_lookup_follows_module_tree_edits() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let lib = src_dir.join("lib.rs");
    std::fs::write(&lib, "pub mod inner { pub struct Model { pub a: i32 } }").unwrap();
    let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());

    bump_epoch();
    let first = get_struct_from_schema_path("crate::inner::Model").expect("found");
    assert!(first.definition.contains("pub a"));

    std::fs::write(
        &lib,
        "pub mod inner { pub struct Model { pub a: i32, pub added_field: bool } }",
    )
    .unwrap();
    bump_epoch();
    let second = get_struct_from_schema_path("crate::inner::Model").expect("found");
    assert!(
        second.definition.contains("added_field"),
        "{}",
        second.definition
    );
}

/// `get_manifest_dir` caches within an epoch but revalidates across epochs,
/// so a long-lived rust-analyzer proc-macro server reused for a DIFFERENT
/// crate (different `CARGO_MANIFEST_DIR`) stops resolving cross-file lookups
/// against the previous crate's `src/`.
#[test]
fn manifest_dir_revalidates_across_epochs() {
    let _env_guard =
        crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", "/vespera_test/crate_a");
    bump_epoch();
    assert_eq!(get_manifest_dir().as_deref(), Some("/vespera_test/crate_a"));

    // Same epoch: cached even though the env changed underneath.
    let _env_guard =
        crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", "/vespera_test/crate_b");
    assert_eq!(
        get_manifest_dir().as_deref(),
        Some("/vespera_test/crate_a"),
        "manifest dir must be cached within an epoch"
    );

    // New epoch: revalidated → picks up the new crate's manifest dir.
    bump_epoch();
    assert_eq!(
        get_manifest_dir().as_deref(),
        Some("/vespera_test/crate_b"),
        "manifest dir must revalidate when the epoch advances"
    );
}

#[test]
fn raw_cache_helpers_reuse_directory_struct_and_content_entries() {
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let file = src_dir.join("model.rs");
    std::fs::write(&file, "pub struct Model { pub id: i32 }").unwrap();

    FILE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        cache.epoch = cache.epoch.wrapping_add(1);
        let first_files = ensure_file_list(&mut cache, &src_dir);
        assert_eq!(first_files.as_ref(), std::slice::from_ref(&file));

        cache.epoch = cache.epoch.wrapping_add(1);
        let second_files = ensure_file_list(&mut cache, &src_dir);
        assert!(Arc::ptr_eq(&first_files, &second_files));

        assert_eq!(
            ensure_struct_definitions(&mut cache, &file),
            Some(StructDefs::Cached)
        );
        let first_content = get_file_content_inner(&mut cache, &file).expect("content exists");
        let second_content =
            get_file_content_inner(&mut cache, &file).expect("cached content exists");
        assert!(Arc::ptr_eq(&first_content, &second_content));
        assert_eq!(cache.struct_definitions[&file].1.len(), 1);
        assert_eq!(
            cache.file_contents[&file].1.as_str(),
            "pub struct Model { pub id: i32 }"
        );
    });
}

/// A file without a fingerprint (metadata unavailable) still answers from
/// its freshly parsed definitions, and a stale cache entry is evicted.
#[test]
fn struct_definitions_without_fingerprint_are_returned_uncached() {
    let temp_dir = TempDir::new().unwrap();
    let file = temp_dir.path().join("model.rs");
    std::fs::write(&file, "pub struct Model { pub id: i32 }").unwrap();

    FILE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        cache.epoch = cache.epoch.wrapping_add(1);
        let epoch = cache.epoch;
        // A stale entry from an earlier read, then "no fingerprint" this epoch.
        cache.struct_definitions.insert(
            file.clone(),
            (
                FileFingerprint {
                    mtime: std::time::SystemTime::UNIX_EPOCH,
                    len: 1,
                },
                HashMap::from([("Stale".to_string(), "struct Stale;".to_string())]),
            ),
        );
        cache.mtime_epoch_cache.insert(file.clone(), (epoch, None));
        let Some(StructDefs::Uncached(defs)) = ensure_struct_definitions(&mut cache, &file) else {
            panic!("expected uncached definitions");
        };
        assert!(defs["Model"].contains("pub id"));
        assert!(!cache.struct_definitions.contains_key(&file));
        assert_eq!(
            ensure_struct_definitions(&mut cache, &temp_dir.path().join("missing.rs")),
            None
        );
        cache.mtime_epoch_cache.insert(file.clone(), (epoch, None));
    });
    // The public lookup reads the uncached definitions too.
    assert!(get_struct_definition(&file, "Model").is_some_and(|def| def.contains("pub id")));
    assert_eq!(get_struct_definition(&file, "Other"), None);
}

/// Inside the compiler (`--crate-name` on the command line, not
/// rust-analyzer) the sources cannot change: a fingerprint and a directory
/// listing are fetched once, not once per macro invocation.  Elsewhere every
/// epoch revalidates them.
#[rstest::rstest]
#[case::compiler("rustc\n--crate-name\napp", 1)]
#[case::other_host("target/debug/deps/tests", 3)]
fn test_stable_sources_skip_revalidation(#[case] args: &str, #[case] stats: usize) {
    let args_guard =
        crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, args);
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("a.rs");
    std::fs::write(&path, "x").unwrap();
    reset_metadata_call_count();
    for _ in 0..3 {
        bump_epoch();
        assert!(get_file_fingerprint(&path).is_some());
    }
    assert_eq!(metadata_call_count(), stats);
    let listed = |cache: &mut FileCache| ensure_file_list(cache, temp.path()).len();
    bump_epoch();
    assert_eq!(FILE_CACHE.with(|cache| listed(&mut cache.borrow_mut())), 1);
    std::fs::write(temp.path().join("b.rs"), "y").unwrap();
    bump_epoch();
    let expected = if stats == 1 { 1 } else { 2 };
    assert_eq!(
        FILE_CACHE.with(|cache| listed(&mut cache.borrow_mut())),
        expected
    );
    drop(args_guard);
    bump_epoch();
}
