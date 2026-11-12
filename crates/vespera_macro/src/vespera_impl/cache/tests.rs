use std::collections::BTreeMap;

use vespera_core::schema::SecuritySchemeType;

use super::*;

fn base_processed() -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        ..ProcessedVesperaInput::default()
    }
}

#[test]
fn test_compute_config_hash_with_servers() {
    let processed_no_servers = base_processed();

    let processed_with_servers = ProcessedVesperaInput {
        servers: Some(vec![
            vespera_core::openapi::Server {
                url: "https://api.example.com".to_string(),
                description: None,
                variables: None,
            },
            vespera_core::openapi::Server {
                url: "http://localhost:3000".to_string(),
                description: None,
                variables: None,
            },
        ]),
        ..base_processed()
    };

    let hash_no_servers = compute_config_hash(&processed_no_servers);
    let hash_with_servers = compute_config_hash(&processed_with_servers);

    // Different servers should produce different hashes
    assert_ne!(
        hash_no_servers, hash_with_servers,
        "Servers should affect config hash"
    );
}

#[test]
fn test_compute_config_hash_with_merge() {
    let processed_no_merge = base_processed();

    let processed_with_merge = ProcessedVesperaInput {
        merge: vec![syn::parse_quote!(app::TestApp)],
        ..base_processed()
    };

    let hash_no_merge = compute_config_hash(&processed_no_merge);
    let hash_with_merge = compute_config_hash(&processed_with_merge);

    assert_ne!(
        hash_no_merge, hash_with_merge,
        "Merge paths should affect config hash"
    );
}

#[test]
fn test_read_cache_corrupt_file_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("routes.cache");
    std::fs::write(&path, "{not valid json").unwrap();
    assert!(read_cache(&path).is_none(), "corrupt cache must be a miss");
}

#[test]
fn test_read_cache_missing_file_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(read_cache(&dir.path().join("nope.cache")).is_none());
}

#[test]
fn test_old_format_cache_deserializes_with_format_zero() {
    // A pre-sidecar cache (inline spec strings, no cache_format
    // field) must still parse — with cache_format defaulting to 0
    // so the orchestrator's `== CACHE_FORMAT` check misses.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("routes.cache");
    let old_format = serde_json::json!({
        "macro_version": "0.1.0",
        "macro_dev_fingerprint": 1u64,
        "file_fingerprints": {},
        "schema_hash": 2u64,
        "config_hash": 3u64,
        "metadata": { "routes": [], "structs": [] },
        "spec_json": "{\"openapi\":\"3.1.0\"}",
        "spec_pretty": "{\n  \"openapi\": \"3.1.0\"\n}"
    });
    std::fs::write(&path, old_format.to_string()).unwrap();
    let cache = read_cache(&path).expect("old format must still deserialize");
    assert_eq!(cache.cache_format, 0, "missing field defaults to 0");
    assert_ne!(cache.cache_format, CACHE_FORMAT, "format check must miss");
    assert!(cache.spec_json_hash.is_none());
    assert!(cache.spec_pretty_hash.is_none());
}

#[test]
fn test_hash_str_deterministic_and_content_sensitive() {
    assert_eq!(hash_str("abc"), hash_str("abc"));
    assert_ne!(hash_str("abc"), hash_str("abd"));
}

#[test]
fn sidecar_matches_accepts_matching_fingerprint_without_hash_miss() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("spec.json");
    std::fs::write(&path, "{\"openapi\":\"3.1.0\"}").unwrap();

    let fingerprint = path_fingerprint(&path);
    assert!(sidecar_matches(&path, Some(0), fingerprint));
}

#[test]
fn sidecar_matches_falls_back_to_hash_when_fingerprint_differs() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("spec.json");
    let content = "{\"openapi\":\"3.1.0\"}";
    std::fs::write(&path, content).unwrap();

    assert!(sidecar_matches(&path, Some(hash_str(content)), Some(1)));
    assert!(!sidecar_matches(&path, Some(hash_str("corrupt")), Some(1)));
}

/// A macro-source edit that PRESERVES the mtime (timestamp-preserving
/// checkout / `cp -p` / build-cache restore) but changes the file size
/// must still move the macro-dev fingerprint — the mtime-only version
/// reported an unchanged fingerprint here and served a stale route cache.
#[test]
fn macro_source_fingerprint_reacts_to_size_only_change() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "fn a() {}").unwrap();
    let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();

    let before = fingerprint_rs_directory(dir.path());

    // Rewrite with a DIFFERENT length, then restore the original mtime.
    std::fs::write(&file, "fn a() {} // grew").unwrap();
    let handle = std::fs::File::options().write(true).open(&file).unwrap();
    handle.set_modified(original_mtime).unwrap();
    drop(handle);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        original_mtime,
        "test precondition: mtime must be unchanged"
    );

    assert_ne!(
        before,
        fingerprint_rs_directory(dir.path()),
        "mtime-preserving size change must change the fingerprint"
    );
}

/// A byte-identical rewrite that also restores the mtime is a genuine
/// cache HIT — the size term must not make the fingerprint unstable.
#[test]
fn macro_source_fingerprint_is_stable_for_identical_content_and_mtime() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "fn a() {}").unwrap();
    let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();

    let before = fingerprint_rs_directory(dir.path());

    std::fs::write(&file, "fn a() {}").unwrap();
    let handle = std::fs::File::options().write(true).open(&file).unwrap();
    handle.set_modified(original_mtime).unwrap();
    drop(handle);

    assert_eq!(
        before,
        fingerprint_rs_directory(dir.path()),
        "unchanged file must keep its fingerprint"
    );
}

#[test]
fn export_config_hash_is_namespaced_by_app_and_folder() {
    let base = compute_export_config_hash("ThirdApp", "routes", "");

    assert_ne!(base, compute_export_config_hash("AdminApp", "routes", ""));
    assert_ne!(base, compute_export_config_hash("ThirdApp", "api", ""));
    assert_ne!(
        base,
        compute_export_config_hash("ThirdApp", "routes", "/api")
    );
}

#[test]
fn security_scheme_field_changes_affect_config_hash() {
    fn scheme(http_scheme: &str) -> SecurityScheme {
        SecurityScheme {
            r#type: SecuritySchemeType::Http,
            description: Some("Auth".to_string()),
            name: None,
            r#in: None,
            scheme: Some(http_scheme.to_string()),
            bearer_format: Some("JWT".to_string()),
            flows: None,
            open_id_connect_url: None,
        }
    }

    let bearer = ProcessedVesperaInput {
        security_schemes: Some(BTreeMap::from([(
            "bearerAuth".to_string(),
            scheme("bearer"),
        )])),
        ..base_processed()
    };
    let basic = ProcessedVesperaInput {
        security_schemes: Some(BTreeMap::from([(
            "bearerAuth".to_string(),
            scheme("basic"),
        )])),
        ..base_processed()
    };

    assert_ne!(compute_config_hash(&bearer), compute_config_hash(&basic));
}

#[test]
fn security_none_and_empty_some_have_distinct_config_hashes() {
    let omitted = base_processed();
    let explicit_empty = ProcessedVesperaInput {
        security: Some(Vec::new()),
        ..base_processed()
    };

    assert_ne!(
        compute_config_hash(&omitted),
        compute_config_hash(&explicit_empty)
    );
}

fn fresh_cache() -> VesperaCache {
    VesperaCache {
        cache_format: CACHE_FORMAT,
        macro_version: "9.9.9".to_string(),
        macro_dev_fingerprint: 11,
        file_fingerprints: HashMap::from([("routes/users.rs".to_string(), 22u64)]),
        schema_hash: 33,
        config_hash: 44,
        metadata: CollectedMetadata::new(),
        spec_json_hash: None,
        spec_pretty_hash: None,
        spec_json_fingerprint: None,
        spec_pretty_fingerprint: None,
    }
}

fn fresh_key(fingerprints: &HashMap<String, u64>) -> CacheKey<'_> {
    CacheKey {
        macro_version: "9.9.9",
        macro_dev_fingerprint: 11,
        file_fingerprints: fingerprints,
        schema_hash: 33,
        config_hash: 44,
    }
}

/// Every invalidation input in the cache header must independently flip
/// freshness — this is the whole reason the predicate lives in one place.
#[rstest::rstest]
#[case::cache_format(|c: &mut VesperaCache| c.cache_format = CACHE_FORMAT + 1)]
#[case::macro_version(|c: &mut VesperaCache| c.macro_version = "9.9.10".to_string())]
#[case::macro_dev_fingerprint(|c: &mut VesperaCache| c.macro_dev_fingerprint = 12)]
#[case::file_fingerprints(|c: &mut VesperaCache| {
    c.file_fingerprints
        .insert("routes/users.rs".to_string(), 23);
})]
#[case::schema_hash(|c: &mut VesperaCache| c.schema_hash = 34)]
#[case::config_hash(|c: &mut VesperaCache| c.config_hash = 45)]
fn is_fresh_flips_on_each_invalidation_input(#[case] mutate: fn(&mut VesperaCache)) {
    let fingerprints = fresh_cache().file_fingerprints;
    let key = fresh_key(&fingerprints);

    let cache = fresh_cache();
    assert!(cache.is_fresh(&key), "unmutated cache must be fresh");

    let mut stale = fresh_cache();
    mutate(&mut stale);
    assert!(
        !stale.is_fresh(&key),
        "mutating one invalidation input must miss the cache"
    );
}

#[test]
fn is_fresh_detects_added_and_removed_route_files() {
    let fingerprints = fresh_cache().file_fingerprints;
    let key = fresh_key(&fingerprints);

    let mut extra_file = fresh_cache();
    extra_file
        .file_fingerprints
        .insert("routes/posts.rs".to_string(), 55);
    assert!(!extra_file.is_fresh(&key));

    let mut no_files = fresh_cache();
    no_files.file_fingerprints.clear();
    assert!(!no_files.is_fresh(&key));
}

#[test]
fn server_description_changes_affect_config_hash() {
    let production = ProcessedVesperaInput {
        servers: Some(vec![vespera_core::openapi::Server {
            url: "https://api.example.com".to_string(),
            description: Some("Production".to_string()),
            variables: None,
        }]),
        ..base_processed()
    };
    let staging = ProcessedVesperaInput {
        servers: Some(vec![vespera_core::openapi::Server {
            url: "https://api.example.com".to_string(),
            description: Some("Staging".to_string()),
            variables: None,
        }]),
        ..base_processed()
    };

    assert_ne!(
        compute_config_hash(&production),
        compute_config_hash(&staging)
    );
}

#[test]
fn sidecar_without_expected_hash_never_matches() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("spec.json");
    std::fs::write(&path, "{}").unwrap();
    assert!(!sidecar_matches(&path, None, path_fingerprint(&path)));
}

#[test]
fn stored_route_signatures_contribute_to_schema_hash() {
    let route = |sig: &str| crate::route_impl::StoredRouteInfo {
        fn_name: "get_user".to_string(),
        method: None,
        custom_path: None,
        success_status: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        file_path: Some("src/routes/users.rs".to_string()),
        fn_sig_str: sig.to_string(),
    };
    let before = with_route_signature_hash(7, &[route("fn get_user () -> Result < A , B >")]);
    let after = with_route_signature_hash(7, &[route("fn get_user () -> Result < A , C >")]);
    assert_ne!(before, after);
    assert_eq!(
        before,
        with_route_signature_hash(7, &[route("fn get_user () -> Result < A , B >")])
    );
    assert_ne!(
        with_route_signature_hash(7, &[]),
        with_route_signature_hash(8, &[])
    );
}

#[test]
fn schema_field_defaults_contribute_to_schema_hash() {
    let without_default = StructMetadata::new(
        "User".to_string(),
        "pub struct User { pub id: i32 }".to_string(),
    );
    let mut with_default = without_default.clone();
    with_default
        .field_defaults
        .insert("id".to_string(), serde_json::json!(7));
    let base = HashMap::from([("User".to_string(), without_default)]);
    let changed = HashMap::from([("User".to_string(), with_default)]);

    assert_ne!(compute_schema_hash(&base), compute_schema_hash(&changed));
}

#[test]
fn security_requirement_names_and_tag_descriptions_affect_config_hash() {
    let secured = ProcessedVesperaInput {
        security: Some(vec![BTreeMap::from([
            ("zeta".to_string(), Vec::new()),
            ("alpha".to_string(), Vec::new()),
        ])]),
        tag_descriptions: Some(HashMap::from([
            ("users".to_string(), "User operations".to_string()),
            ("admin".to_string(), "Admin operations".to_string()),
        ])),
        ..base_processed()
    };

    assert_ne!(
        compute_config_hash(&base_processed()),
        compute_config_hash(&secured)
    );
}

/// Changing only a requirement's scopes must invalidate the cache.
#[rstest::rstest]
#[case::added_scope(vec!["read"], vec!["read", "write"])]
#[case::replaced_scope(vec!["read"], vec!["admin"])]
#[case::empty_vs_some(vec![], vec!["read"])]
fn security_requirement_scopes_affect_config_hash(
    #[case] before: Vec<&str>,
    #[case] after: Vec<&str>,
) {
    let with_scopes = |scopes: Vec<&str>| ProcessedVesperaInput {
        security: Some(vec![BTreeMap::from([(
            "oauth2".to_string(),
            scopes.into_iter().map(str::to_string).collect(),
        )])]),
        ..base_processed()
    };
    assert_ne!(
        compute_config_hash(&with_scopes(before)),
        compute_config_hash(&with_scopes(after))
    );
}

#[test]
fn present_merge_sidecar_content_contributes_to_config_hash() {
    let temp = tempfile::TempDir::new().unwrap();
    let vespera_dir = temp.path().join("target/vespera");
    std::fs::create_dir_all(&vespera_dir).unwrap();
    std::fs::write(vespera_dir.join("Child.openapi.json"), "child-v1").unwrap();
    let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
    let processed = ProcessedVesperaInput {
        merge: vec![syn::parse_quote!(child::Child)],
        ..base_processed()
    };
    let mut cache = MergeSpecCache::new();

    let with_child = compute_config_hash_with_merge_cache(&processed, &mut cache);
    assert_ne!(with_child, compute_config_hash(&base_processed()));
}

#[test]
fn macro_source_fingerprint_is_zero_for_an_absent_directory() {
    let temp = tempfile::TempDir::new().unwrap();
    assert_eq!(fingerprint_rs_directory(&temp.path().join("absent")), 0);
}

#[rstest::rstest]
#[case::macro_source("vespera_macro/src/lib.rs")]
#[case::core_source("vespera_core/src/openapi.rs")]
fn macro_dev_fingerprint_tracks_macro_and_core_sources(#[case] edited: &str) {
    let temp = tempfile::TempDir::new().unwrap();
    let crates = temp.path().join("crates");
    for file in ["vespera_macro/src/lib.rs", "vespera_core/src/openapi.rs"] {
        let path = crates.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "pub fn first() {}").unwrap();
    }
    let macro_dir = crates.join("vespera_macro");

    let before = macro_dev_fingerprint_for(&macro_dir);
    std::fs::write(
        crates.join(edited),
        "pub fn first() { let changed = true; }",
    )
    .unwrap();

    assert_ne!(
        before,
        macro_dev_fingerprint_for(&macro_dir),
        "editing {edited} must move the digest"
    );
}

#[test]
fn packaged_macro_fingerprint_is_zero() {
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/lib.rs"), "pub fn f() {}").unwrap();
    std::fs::write(temp.path().join("Cargo.toml.orig"), "[package]").unwrap();
    assert_eq!(macro_dev_fingerprint_for(temp.path()), 0);
}

#[test]
fn in_repo_macro_fingerprint_covers_its_own_sources() {
    assert_ne!(compute_macro_dev_fingerprint(), 0);
    assert_eq!(
        macro_src_dirs(Path::new(env!("CARGO_MANIFEST_DIR"))).map(|dir| dir.is_dir()),
        [true, true],
        "both the macro and the vespera_core sources must be located"
    );
}

#[test]
fn rs_fingerprints_skip_other_extensions_and_survive_nesting() {
    let temp = tempfile::TempDir::new().unwrap();
    let nested = temp.path().join("routes");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(temp.path().join("lib.rs"), "pub struct App;").unwrap();
    std::fs::write(temp.path().join("README.md"), "fixture").unwrap();
    std::fs::write(nested.join("users.rs"), "pub fn list() {}").unwrap();

    let scanned = collect_files_with_mtimes(temp.path()).unwrap();
    let rust_only: Vec<_> = scanned
        .iter()
        .filter(|(path, _)| path.extension().is_some_and(|e| e == "rs"))
        .collect();

    assert_eq!(scanned.len(), 3, "the walk must recurse and see every file");
    assert_eq!(rust_only.len(), 2, "both `.rs` files must be fingerprinted");
    assert!(
        rust_only.iter().all(|(_, fingerprint)| *fingerprint != 0),
        "every `.rs` file must carry a real fingerprint: {rust_only:?}"
    );
    assert_ne!(
        hash_rs_fingerprints(scanned.clone()),
        hash_rs_fingerprints(
            scanned
                .into_iter()
                .filter(|(path, _)| !path.ends_with("users.rs"))
                .collect()
        ),
        "dropping a nested `.rs` file must move the digest"
    );
}
