//! Unit tests for `vespera_impl::openapi_io` (sidecar to keep the parent file
//! under the 1000-line cap; `use super::*` semantics preserved).

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::vespera_impl::cache::hash_str;

#[test]
fn test_generate_and_write_openapi_no_output() {
    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: None,
        version: None,
        docs_url: None,
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.spec_json.is_none());
    assert!(result.spec_pretty.is_none());
}

#[test]
fn test_generate_and_write_openapi_docs_only() {
    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: Some("Test API".to_string()),
        version: Some("1.0.0".to_string()),
        docs_url: Some("/docs".to_string()),
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.spec_pretty.is_none());
    assert!(result.spec_json.is_some());
    let json = result.spec_json.unwrap();
    assert!(json.contains("\"openapi\""));
    assert!(json.contains("Test API"));
}

#[test]
fn test_generate_and_write_openapi_redoc_only() {
    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: None,
        version: None,
        docs_url: None,
        redoc_url: Some("/redoc".to_string()),
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.spec_json.is_some());
    assert!(result.spec_pretty.is_none());
}

#[test]
fn test_generate_and_write_openapi_both_docs() {
    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: None,
        version: None,
        docs_url: Some("/docs".to_string()),
        redoc_url: Some("/redoc".to_string()),
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.spec_json.is_some());
    assert!(result.spec_pretty.is_none());
}

#[test]
fn test_generate_and_write_openapi_file_output() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("test-openapi.json");

    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![output_path.to_string_lossy().to_string()],
        title: Some("File Test".to_string()),
        version: Some("2.0.0".to_string()),
        docs_url: None,
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());

    // Verify file was written
    assert!(output_path.exists());
    let content = fs::read_to_string(&output_path).unwrap();
    assert!(content.contains("\"openapi\""));
    assert!(content.contains("File Test"));
    assert!(content.contains("2.0.0"));
}

#[test]
fn test_generate_and_write_openapi_creates_directories() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("nested/dir/openapi.json");

    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![output_path.to_string_lossy().to_string()],
        title: None,
        version: None,
        docs_url: None,
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_ok());

    // Verify nested directories and file were created
    assert!(output_path.exists());
}

#[serial_test::serial]
#[test]
fn test_generate_and_write_openapi_with_merge_no_manifest_dir() {
    // When CARGO_MANIFEST_DIR is not set or merge is empty, it should work normally
    let old_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This serial test temporarily removes process environment to
    // exercise the no-manifest fallback branch.
    unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") };

    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: Some("Test".to_string()),
        version: None,
        docs_url: Some("/docs".to_string()),
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![syn::parse_quote!(app::TestApp)], // Has merge but no valid manifest dir
    };
    let metadata = CollectedMetadata::new();
    // This should still work - merge logic is skipped when CARGO_MANIFEST_DIR lookup fails
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    if let Some(value) = old_manifest_dir {
        // SAFETY: This serial test restores the process environment it changed.
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", value) };
    }
    assert!(result.is_ok());
}

#[serial_test::serial]
#[test]
fn test_generate_and_write_openapi_with_merge_and_valid_spec() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create the vespera directory with a spec file
    let target_dir = temp_dir.path().join("target").join("vespera");
    fs::create_dir_all(&target_dir).expect("Failed to create target/vespera dir");

    // Write a valid OpenAPI spec file
    let spec_content =
        r#"{"openapi":"3.1.0","info":{"title":"Child API","version":"1.0.0"},"paths":{}}"#;
    fs::write(target_dir.join("ChildApp.openapi.json"), spec_content)
        .expect("Failed to write spec file");

    // Save and set CARGO_MANIFEST_DIR
    let old_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: We're in a single-threaded test context
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![],
        title: Some("Parent API".to_string()),
        version: Some("2.0.0".to_string()),
        docs_url: Some("/docs".to_string()),
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![syn::parse_quote!(child::ChildApp)],
    };
    let metadata = CollectedMetadata::new();

    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );

    // Restore CARGO_MANIFEST_DIR
    if let Some(old_value) = old_manifest_dir {
        // SAFETY: We're in a single-threaded test context
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", old_value) };
    }

    assert!(result.is_ok());
}

#[serial_test::serial]
#[test]
fn merged_child_schema_conflict_is_a_spanned_compile_error() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target/vespera");
    fs::create_dir_all(&target_dir).unwrap();
    let spec = |property: &str, schema_type: &str| {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "child", "version": "1.0.0" },
            "paths": {},
            "components": {
                "schemas": {
                    "ExampleItem": {
                        "type": "object",
                        "properties": { (property): { "type": schema_type } }
                    }
                }
            }
        })
        .to_string()
    };
    fs::write(
        target_dir.join("PluginA.openapi.json"),
        spec("id", "string"),
    )
    .unwrap();
    fs::write(
        target_dir.join("PluginB.openapi.json"),
        spec("collisionMarker", "boolean"),
    )
    .unwrap();

    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let mut processed = merge_input(syn::parse_quote!(plugin_a::PluginA));
    processed.merge.push(syn::parse_quote!(plugin_b::PluginB));
    let error = generate_and_write_openapi(
        &processed,
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    )
    .expect_err("different same-named child schemas must fail the build");
    let message = error.to_string();
    assert!(message.contains("Duplicate OpenAPI schema name 'ExampleItem'"));
    assert!(message.contains("plugin_a::PluginA"));
    assert!(message.contains("plugin_b::PluginB"));
}

#[test]
fn test_generate_and_write_openapi_file_write_error() {
    // Line 95: fs::write failure when output path is a directory
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a directory where the output file should be
    let output_path = temp_dir.path().join("openapi.json");
    fs::create_dir(&output_path).expect("Failed to create directory");

    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: vec![output_path.to_string_lossy().to_string()],
        title: Some("Test API".to_string()),
        version: Some("1.0.0".to_string()),
        docs_url: None,
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![],
    };
    let metadata = CollectedMetadata::new();

    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("failed to write file"));
}

#[test]
fn test_ensure_openapi_files_from_cache_none_spec() {
    // Exercises lines 266-267: early return when spec_pretty is None
    let result = ensure_openapi_files_from_cache(&["dummy.json".to_string()], None);
    assert!(result.is_ok());
}

#[test]
fn test_ensure_openapi_files_from_cache_writes_file() {
    // Exercises lines 269-276: write new file
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("api.json");
    let spec = r#"{"openapi":"3.1.0"}"#;

    let result =
        ensure_openapi_files_from_cache(&[output_path.to_string_lossy().to_string()], Some(spec));
    assert!(result.is_ok());
    assert_eq!(fs::read_to_string(&output_path).unwrap(), spec);
}

#[test]
fn test_ensure_openapi_files_from_cache_skip_unchanged() {
    // Exercises line 271-272: should_write is false when content matches
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("api.json");
    let spec = r#"{"openapi":"3.1.0"}"#;

    // Write file first with same content
    fs::write(&output_path, spec).unwrap();

    let result =
        ensure_openapi_files_from_cache(&[output_path.to_string_lossy().to_string()], Some(spec));
    assert!(result.is_ok());
    // File should still contain same content (no unnecessary write)
    assert_eq!(fs::read_to_string(&output_path).unwrap(), spec);
}

#[test]
fn test_ensure_openapi_files_from_cache_creates_parent_dirs() {
    // Exercises lines 273-274: create parent directories
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("nested").join("dir").join("api.json");
    let spec = r#"{"openapi":"3.1.0"}"#;

    let result =
        ensure_openapi_files_from_cache(&[output_path.to_string_lossy().to_string()], Some(spec));
    assert!(result.is_ok());
    assert!(output_path.exists());
    assert_eq!(fs::read_to_string(&output_path).unwrap(), spec);
}

#[test]
fn test_ensure_openapi_files_from_cache_write_error() {
    // Exercises line 276: write failure
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_path = temp_dir.path().join("api.json");

    // Create a directory where the file should be -> write will fail
    fs::create_dir(&output_path).unwrap();

    let result =
        ensure_openapi_files_from_cache(&[output_path.to_string_lossy().to_string()], Some("spec"));
    assert!(result.is_err());
}

#[test]
fn test_ensure_openapi_files_from_cache_multiple_files() {
    // Exercises the loop with multiple file names (line 269)
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let path1 = temp_dir.path().join("api1.json");
    let path2 = temp_dir.path().join("api2.json");
    let spec = r#"{"openapi":"3.1.0"}"#;

    let result = ensure_openapi_files_from_cache(
        &[
            path1.to_string_lossy().to_string(),
            path2.to_string_lossy().to_string(),
        ],
        Some(spec),
    );
    assert!(result.is_ok());
    assert_eq!(fs::read_to_string(&path1).unwrap(), spec);
    assert_eq!(fs::read_to_string(&path2).unwrap(), spec);
}

struct RestoreManifest(Option<String>);

impl Drop for RestoreManifest {
    fn drop(&mut self) {
        // SAFETY: environment-mutating tests are serialized and restore their prior value.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                None => std::env::remove_var("CARGO_MANIFEST_DIR"),
            }
        }
    }
}

fn merge_input(path: syn::Path) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        openapi_file_names: Vec::new(),
        title: None,
        version: None,
        docs_url: Some("/docs".to_string()),
        redoc_url: None,
        servers: None,
        security_schemes: None,
        security: None,
        tag_descriptions: None,
        merge: vec![path],
    }
}

#[test]
#[serial_test::serial]
fn merge_reports_missing_and_malformed_child_specs() {
    let temp = TempDir::new().unwrap();
    let vespera_dir = temp.path().join("target/vespera");
    fs::create_dir_all(&vespera_dir).unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };

    let missing = generate_and_write_openapi(
        &merge_input(syn::parse_quote!(child::MissingChild)),
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    )
    .expect_err("missing child sidecar must fail");
    assert!(missing.to_string().contains("failed to read child spec"));

    fs::write(vespera_dir.join("BrokenChild.openapi.json"), "not-json").unwrap();
    let malformed = generate_and_write_openapi(
        &merge_input(syn::parse_quote!(child::BrokenChild)),
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    )
    .expect_err("malformed child sidecar must fail");
    assert!(malformed.to_string().contains("failed to parse child spec"));
}

#[test]
fn ensure_cached_openapi_reports_parent_creation_error() {
    let temp = TempDir::new().unwrap();
    let blocking_parent = temp.path().join("blocking");
    fs::write(&blocking_parent, "file").unwrap();
    let output = blocking_parent.join("openapi.json");

    let error =
        ensure_openapi_files_from_cache(&[output.to_string_lossy().into_owned()], Some("{}"))
            .expect_err("a file cannot be created as a directory");
    assert!(error.to_string().contains("failed to create directory"));
}

#[test]
#[serial_test::serial]
fn sidecar_validation_covers_missing_fingerprints_corruption_and_success() {
    let temp = TempDir::new().unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };
    let vespera_dir = temp.path().join("target/vespera");
    fs::create_dir_all(&vespera_dir).unwrap();
    let compact = embed_spec_path();
    let pretty = pretty_sidecar_path();
    fs::write(&compact, "compact").unwrap();
    fs::write(&pretty, "pretty").unwrap();
    let compact_fp = path_fingerprint(&compact).unwrap();
    let pretty_fp = path_fingerprint(&pretty).unwrap();

    assert!(load_validated_sidecar_specs(Some(hash_str("compact")), None, None, None).is_none());
    assert!(load_validated_sidecar_specs(None, Some(hash_str("pretty")), None, None).is_none());
    assert!(
        load_validated_sidecar_specs(
            Some(hash_str("wrong")),
            None,
            Some(compact_fp.wrapping_add(1)),
            None,
        )
        .is_none()
    );
    assert!(
        load_validated_sidecar_specs(
            None,
            Some(hash_str("wrong")),
            None,
            Some(pretty_fp.wrapping_add(1)),
        )
        .is_none()
    );

    let loaded = load_validated_sidecar_specs(
        Some(hash_str("compact")),
        Some(hash_str("pretty")),
        Some(compact_fp),
        Some(pretty_fp),
    )
    .expect("matching sidecars load");
    assert_eq!(loaded.pretty.as_deref(), Some("pretty"));
    assert!(loaded.spec_tokens.is_some());
}

#[test]
fn sidecar_validation_accepts_cache_without_generated_specs() {
    let loaded = load_validated_sidecar_specs(None, None, None, None)
        .expect("a cache without configured specs needs no sidecar validation");

    assert!(loaded.pretty.is_none());
    assert!(loaded.spec_tokens.is_none());
}

#[test]
#[serial_test::serial]
fn embedding_reports_directory_creation_and_spec_write_errors() {
    let create_temp = TempDir::new().unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", create_temp.path()) };
    fs::create_dir_all(create_temp.path().join("target")).unwrap();
    fs::write(create_temp.path().join("target/vespera"), "blocking file").unwrap();
    let Err(create_error) = write_spec_for_embedding(Some("{}".to_string())) else {
        panic!("blocking parent must fail directory creation");
    };
    assert!(
        create_error
            .to_string()
            .contains("failed to create directory")
    );

    let write_temp = TempDir::new().unwrap();
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", write_temp.path()) };
    let spec_path = embed_spec_path();
    fs::create_dir_all(&spec_path).unwrap();
    let Err(write_error) = write_spec_for_embedding(Some("{}".to_string())) else {
        panic!("a directory cannot be overwritten as a spec file");
    };
    assert!(
        write_error
            .to_string()
            .contains("failed to write spec file")
    );
}

#[serial_test::serial]
#[test]
fn merged_children_with_same_operation_id_is_a_compile_error() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target/vespera");
    fs::create_dir_all(&target_dir).unwrap();
    let spec = |path: &str| {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "child", "version": "1.0.0" },
            "paths": {
                (path): { "get": { "operationId": "health", "responses": {} } }
            }
        })
        .to_string()
    };
    fs::write(target_dir.join("PluginA.openapi.json"), spec("/a/health")).unwrap();
    fs::write(target_dir.join("PluginB.openapi.json"), spec("/b/health")).unwrap();

    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let mut processed = merge_input(syn::parse_quote!(plugin_a::PluginA));
    processed.merge.push(syn::parse_quote!(plugin_b::PluginB));
    let message = generate_and_write_openapi(
        &processed,
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    )
    .expect_err("a child re-using an operationId must fail the build")
    .to_string();
    insta::assert_snapshot!("merged_duplicate_operation_id_error", message);
}

/// A parent route may reference a security scheme that only a merged child
/// declares: validation runs against the MERGED `components.securitySchemes`.
/// A name declared nowhere is still rejected.
#[rstest::rstest]
#[serial_test::serial]
#[case::declared_by_child("childAuth", true)]
#[case::declared_nowhere("ghostAuth", false)]
fn security_references_are_validated_after_merge(#[case] scheme: &str, #[case] ok: bool) {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target/vespera");
    fs::create_dir_all(&target_dir).unwrap();
    let child = serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "child", "version": "1.0.0" },
        "paths": {},
        "components": {
            "securitySchemes": { "childAuth": { "type": "http", "scheme": "bearer" } }
        }
    });
    fs::write(target_dir.join("PluginA.openapi.json"), child.to_string()).unwrap();

    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
    let mut processed = merge_input(syn::parse_quote!(plugin_a::PluginA));
    processed.security_schemes = Some(std::collections::BTreeMap::from([(
        "parentAuth".to_string(),
        serde_json::from_value(serde_json::json!({ "type": "http", "scheme": "basic" })).unwrap(),
    )]));
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(crate::metadata::RouteMetadata {
        method: "get".to_string(),
        path: "/secure".to_string(),
        function_name: "secure".to_string(),
        module_path: "routes::secure".to_string(),
        file_path: "virtual/secure.rs".to_string(),
        success_status: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: Some(vec![scheme.to_string()]),
        headers: Vec::new(),
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
    });
    let result = generate_and_write_openapi(
        &processed,
        &metadata,
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    match result {
        Ok(_) => assert!(ok, "`{scheme}` should have been rejected"),
        Err(error) => {
            assert!(!ok, "`{scheme}` should be accepted: {error}");
            insta::assert_snapshot!("merged_security_reference_unknown_error", error.to_string());
        }
    }
}
