use std::{collections::HashMap, path::Path};

use crate::{
    error::{MacroResult, err_call_site},
    metadata::CollectedMetadata,
    openapi_generator::generate_openapi_doc_with_metadata,
    route_impl::StoredRouteInfo,
    router_codegen::ProcessedVesperaInput,
};
use proc_macro2::Span;

use super::path_utils::{current_crate_tag, find_target_dir};

/// Docs info tuple type alias for cleaner signatures
pub type DocsInfo = (Option<String>, Option<String>, Option<String>);

/// Generate `OpenAPI` JSON and write to files, returning docs info
pub fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
    file_asts: HashMap<String, syn::File>,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<DocsInfo> {
    if input.openapi_file_names.is_empty() && input.docs_url.is_none() && input.redoc_url.is_none()
    {
        return Ok((None, None, None));
    }

    let mut openapi_doc = generate_openapi_doc_with_metadata(
        input.title.clone(),
        input.version.clone(),
        input.servers.clone(),
        metadata,
        Some(file_asts),
        route_storage,
    );

    // Merge specs from child apps at compile time
    if !input.merge.is_empty()
        && let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR")
    {
        let manifest_path = Path::new(&manifest_dir);
        let target_dir = find_target_dir(manifest_path);
        let vespera_dir = target_dir.join("vespera");

        for merge_path in &input.merge {
            // Extract the struct name (last segment, e.g., "ThirdApp" from "third::ThirdApp")
            if let Some(last_segment) = merge_path.segments.last() {
                let struct_name = last_segment.ident.to_string();
                let spec_file = vespera_dir.join(format!("{struct_name}.openapi.json"));

                if let Ok(spec_content) = std::fs::read_to_string(&spec_file)
                    && let Ok(child_spec) =
                        serde_json::from_str::<vespera_core::openapi::OpenApi>(&spec_content)
                {
                    openapi_doc.merge(child_spec);
                }
            }
        }
    }

    // NOTE on F-01: an earlier audit suggested serialising the
    // `OpenApi` document once into `serde_json::Value` and emitting
    // pretty + compact from the cached `Value`.  We deliberately do
    // **not** do that here.  Going through `Value` re-orders every
    // object's keys alphabetically (because the default
    // `serde_json::Map` is `BTreeMap`-backed), which silently changes
    // the field order in every user-visible `openapi.json` file.  The
    // marginal build-time saving is not worth churning the output of a
    // file users diff in CI.  Keep two direct serialisations.
    //
    // Pretty-print for user-visible files.
    if !input.openapi_file_names.is_empty() {
        let json_pretty = serde_json::to_string_pretty(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?;
        for openapi_file_name in &input.openapi_file_names {
            let file_path = Path::new(openapi_file_name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err_call_site(format!("OpenAPI output: failed to create directory '{}'. Error: {}. Ensure the path is valid and writable.", parent.display(), e)))?;
            }
            let should_write =
                std::fs::read_to_string(file_path).map_or(true, |existing| existing != json_pretty);
            if should_write {
                std::fs::write(file_path, &json_pretty).map_err(|e| err_call_site(format!("OpenAPI output: failed to write file '{openapi_file_name}'. Error: {e}. Ensure the file path is writable.")))?;
            }
        }
    }

    // Compact JSON for embedding (smaller binary, faster downstream compilation).
    let spec_json = if input.docs_url.is_some() || input.redoc_url.is_some() {
        Some(serde_json::to_string(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?)
    } else {
        None
    };

    Ok((input.docs_url.clone(), input.redoc_url.clone(), spec_json))
}

/// Write cached OpenAPI spec to output files if they are stale or missing.
pub fn ensure_openapi_files_from_cache(
    openapi_file_names: &[String],
    spec_pretty: Option<&str>,
) -> syn::Result<()> {
    let Some(pretty) = spec_pretty else {
        return Ok(());
    };
    for openapi_file_name in openapi_file_names {
        let file_path = Path::new(openapi_file_name);
        let should_write =
            std::fs::read_to_string(file_path).map_or(true, |existing| existing != *pretty);
        if should_write {
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    syn::Error::new(
                        Span::call_site(),
                        format!(
                            "OpenAPI output: failed to create directory '{}': {}",
                            parent.display(),
                            e
                        ),
                    )
                })?;
            }
            std::fs::write(file_path, pretty).map_err(|e| {
                syn::Error::new(
                    Span::call_site(),
                    format!("OpenAPI output: failed to write file '{openapi_file_name}': {e}"),
                )
            })?;
        }
    }
    Ok(())
}

/// Write compact spec JSON to target dir for `include_str!` embedding.
///
/// The file name is **namespaced per crate**: two workspace members
/// both using `vespera!` compile in parallel under the same shared
/// `target/vespera/` directory — with a single shared file name, crate
/// A's `include_str!` could read the spec crate B just wrote.
pub(super) fn write_spec_for_embedding(
    spec_json: Option<String>,
) -> syn::Result<Option<proc_macro2::TokenStream>> {
    let Some(json) = spec_json else {
        return Ok(None);
    };
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    let target_dir = find_target_dir(manifest_path);
    let vespera_dir = target_dir.join("vespera");
    std::fs::create_dir_all(&vespera_dir).map_err(|e| {
        syn::Error::new(
            Span::call_site(),
            format!(
                "vespera! macro: failed to create directory '{}': {}",
                vespera_dir.display(),
                e
            ),
        )
    })?;
    let spec_file = vespera_dir.join(format!("vespera_spec-{}.json", current_crate_tag()));
    let should_write =
        std::fs::read_to_string(&spec_file).map_or(true, |existing| existing != json);
    if should_write {
        std::fs::write(&spec_file, &json).map_err(|e| {
            syn::Error::new(
                Span::call_site(),
                format!(
                    "vespera! macro: failed to write spec file '{}': {}",
                    spec_file.display(),
                    e
                ),
            )
        })?;
    }
    let path_str = spec_file.display().to_string().replace('\\', "/");
    Ok(Some(quote::quote! { include_str!(#path_str) }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let (docs_url, redoc_url, spec_json) = result.unwrap();
        assert!(docs_url.is_none());
        assert!(redoc_url.is_none());
        assert!(spec_json.is_none());
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let (docs_url, redoc_url, spec_json) = result.unwrap();
        assert!(docs_url.is_some());
        assert_eq!(docs_url.unwrap(), "/docs");
        assert!(spec_json.is_some());
        let json = spec_json.unwrap();
        assert!(json.contains("\"openapi\""));
        assert!(json.contains("Test API"));
        assert!(redoc_url.is_none());
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let (docs_url, redoc_url, spec_json) = result.unwrap();
        assert!(docs_url.is_none());
        assert!(redoc_url.is_some());
        assert_eq!(redoc_url.unwrap(), "/redoc");
        assert!(spec_json.is_some());
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let (docs_url, redoc_url, spec_json) = result.unwrap();
        assert!(docs_url.is_some());
        assert!(redoc_url.is_some());
        assert!(spec_json.is_some());
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());

        // Verify nested directories and file were created
        assert!(output_path.exists());
    }

    #[test]
    fn test_generate_and_write_openapi_with_merge_no_manifest_dir() {
        // When CARGO_MANIFEST_DIR is not set or merge is empty, it should work normally
        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: Some("Test".to_string()),
            version: None,
            docs_url: Some("/docs".to_string()),
            redoc_url: None,
            servers: None,
            merge: vec![syn::parse_quote!(app::TestApp)], // Has merge but no valid manifest dir
        };
        let metadata = CollectedMetadata::new();
        // This should still work - merge logic is skipped when CARGO_MANIFEST_DIR lookup fails
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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
            merge: vec![syn::parse_quote!(child::ChildApp)],
        };
        let metadata = CollectedMetadata::new();

        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);

        // Restore CARGO_MANIFEST_DIR
        if let Some(old_value) = old_manifest_dir {
            // SAFETY: We're in a single-threaded test context
            unsafe { std::env::set_var("CARGO_MANIFEST_DIR", old_value) };
        }

        assert!(result.is_ok());
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
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();

        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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

        let result = ensure_openapi_files_from_cache(
            &[output_path.to_string_lossy().to_string()],
            Some(spec),
        );
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

        let result = ensure_openapi_files_from_cache(
            &[output_path.to_string_lossy().to_string()],
            Some(spec),
        );
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

        let result = ensure_openapi_files_from_cache(
            &[output_path.to_string_lossy().to_string()],
            Some(spec),
        );
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

        let result = ensure_openapi_files_from_cache(
            &[output_path.to_string_lossy().to_string()],
            Some("spec"),
        );
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
}
