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

/// In a long-lived rust-analyzer proc-macro server the path-keyed lookup
/// caches must not outlive the epoch that populated them — otherwise a
/// model file edited between two macro invocations would keep returning a
/// stale `StructMetadata` / FK result. Advancing the epoch must drop them.
#[serial_test::serial]
#[test]
fn path_lookup_caches_invalidate_across_epochs() {
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
    // Advancing the epoch (the next macro invocation) must drop the
    // path-keyed caches; the next lookup triggers the lazy clear.
    bump_epoch();
    let _ = get_fk_column("ra::trigger::Schema", "Rel");
    assert!(
        !fk_lookup_contains("ra::stale::Schema", "Rel"),
        "stale lookup entry must be invalidated when the epoch advances"
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

/// H1 benchmark + regression: when a single file is added to a directory
/// (the common rust-analyzer edit between two macro invocations), the
/// struct-index rebuild must re-tokenise ONLY the changed file — not every
/// file in the directory.
///
/// `extract_struct_names` (the per-file source tokeniser) is the dominant
/// cost of the rebuild that fires whenever the directory fingerprint changes.
/// Before the per-file name cache the rebuild re-tokenised all N files on
/// every edit; after it, only the new/changed file is re-scanned. The
/// tokenisation count is deterministic, so it is the noise-free signal for
/// this compile-time win (printed as `VESPERA_H1 ...`).
#[serial_test::serial]
#[test]
fn h1_single_file_add_reextracts_only_changed_file() {
    const N: usize = 20;
    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path();

    for i in 0..N {
        std::fs::write(
            src_dir.join(format!("model_{i}.rs")),
            format!("pub struct Model{i} {{ pub id: i32 }}"),
        )
        .unwrap();
    }

    // Cold index build — tokenises every file once (both before and after
    // the fix; the win is on the incremental rebuild below).
    reset_extract_struct_names_count();
    bump_epoch();
    let first = get_struct_candidates(src_dir, "Model0");
    assert_eq!(first.len(), 1, "Model0 must be indexed");
    let initial_build = extract_struct_names_count();

    // Add ONE new file and advance the epoch: the directory fingerprint
    // changes, so the struct index is dropped and rebuilt on the next query.
    std::fs::write(
        src_dir.join("model_new.rs"),
        "pub struct ModelNew { pub id: i32 }",
    )
    .unwrap();
    reset_extract_struct_names_count();
    bump_epoch();
    let added = get_struct_candidates(src_dir, "ModelNew");
    let rebuild = extract_struct_names_count();

    eprintln!(
        "VESPERA_H1 N={N} initial_build_tokenisations={initial_build} \
         single_add_rebuild_tokenisations={rebuild}"
    );

    assert_eq!(added.len(), 1, "newly added ModelNew must be indexed");
    // Correctness: pre-existing structs survive the rebuild.
    assert_eq!(
        get_struct_candidates(src_dir, "Model0").len(),
        1,
        "Model0 must remain reachable after the rebuild"
    );
    // The win: only the newly added file is re-tokenised, not all N+1.
    assert_eq!(
        rebuild, 1,
        "rebuild after a single-file add must re-tokenise only the new file \
         (got {rebuild}; pre-fix this re-tokenised all N+1 = {} files)",
        N + 1
    );
}
