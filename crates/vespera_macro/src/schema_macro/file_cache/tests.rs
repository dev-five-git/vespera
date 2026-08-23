use tempfile::TempDir;

use super::*;

#[test]
fn test_get_fk_column_cache_hit() {
    let result1 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
    let result2 = get_fk_column("nonexistent::path::Schema", "SomeRelation");
    assert_eq!(result1, result2);
}

/// Path-keyed lookup caches survive epoch bumps so repeated `schema_type!`
/// expansions in one crate share path resolution work. Staleness is guarded
/// by the lower file-content / struct-definition mtime caches.
#[serial_test::serial]
#[test]
fn path_lookup_caches_survive_epoch_bumps() {
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
}

#[serial_test::serial]
#[test]
fn path_lookup_revalidates_when_resolved_file_mtime_changes() {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("CARGO_MANIFEST_DIR", v) },
                None => unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") },
            }
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let models_dir = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    let model_path = models_dir.join("user.rs");
    std::fs::write(&model_path, "pub struct Model { pub id: i32 }").unwrap();

    let _restore = Restore(std::env::var("CARGO_MANIFEST_DIR").ok());
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

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

#[serial_test::serial]
#[test]
fn test_missing_file_content_is_negative_cached_within_epoch() {
    let temp_dir = TempDir::new().unwrap();
    let missing_path = temp_dir.path().join("missing.rs");

    reset_metadata_call_count();
    bump_epoch();
    let before = metadata_call_count();

    assert!(get_struct_definition(&missing_path, "Missing").is_none());
    assert!(get_struct_definition(&missing_path, "Missing").is_none());

    assert_eq!(
        metadata_call_count() - before,
        1,
        "missing file should be stat'd once in one epoch"
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

/// Within a single epoch, repeated single-segment path lookups must not
/// rewalk the `src` tree. `path_lookup_fingerprint` folds every `.rs` file
/// under `src` into the digest via `ensure_file_list`, so the first lookup
/// walks + stats; subsequent lookups in the same epoch reuse the validated
/// `DirEntry` (and the per-epoch fingerprint cache) with zero new
/// `fs::metadata` syscalls.
#[serial_test::serial]
#[test]
fn test_file_list_skips_walk_within_same_epoch() {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("CARGO_MANIFEST_DIR", v) },
                None => unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") },
            }
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.rs"), "pub struct Alpha { pub id: i32 }").unwrap();
    std::fs::write(src_dir.join("b.rs"), "pub struct Beta { pub name: String }").unwrap();

    let _restore = Restore(std::env::var("CARGO_MANIFEST_DIR").ok());
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

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

/// `get_manifest_dir` caches within an epoch but revalidates across epochs,
/// so a long-lived rust-analyzer proc-macro server reused for a DIFFERENT
/// crate (different `CARGO_MANIFEST_DIR`) stops resolving cross-file lookups
/// against the previous crate's `src/`.
#[serial_test::serial]
#[test]
fn manifest_dir_revalidates_across_epochs() {
    // Restore the (load-bearing) env var even if an assertion panics.
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("CARGO_MANIFEST_DIR", v) },
                None => unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") },
            }
        }
    }
    let _restore = Restore(std::env::var("CARGO_MANIFEST_DIR").ok());

    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", "/vespera_test/crate_a") };
    bump_epoch();
    assert_eq!(get_manifest_dir().as_deref(), Some("/vespera_test/crate_a"));

    // Same epoch: cached even though the env changed underneath.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", "/vespera_test/crate_b") };
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

        assert!(ensure_struct_definitions(&mut cache, &file));
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
