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
    let first =
        get_struct_from_schema_path("crate::models::user::Model").expect("fixture model resolves");
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
