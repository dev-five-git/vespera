use std::{collections::HashMap, path::Path};

use crate::{
    error::{MacroResult, err_call_site},
    metadata::CollectedMetadata,
    openapi_generator::{OpenApiSecurity, try_generate_openapi_doc_with_metadata},
    route_impl::StoredRouteInfo,
    router_codegen::ProcessedVesperaInput,
};
use proc_macro2::Span;

use super::path_utils::{current_crate_tag, find_target_dir};

/// OpenAPI write result consumed by router/doc codegen and incremental cache sidecars.
#[derive(Debug)]
#[allow(dead_code)]
pub struct OpenApiWriteResult {
    pub docs_url: Option<String>,
    pub redoc_url: Option<String>,
    pub spec_json: Option<String>,
    pub spec_pretty: Option<String>,
}

/// Whether `path` already holds exactly `content`.
///
/// A cheap `metadata().len()` pre-check skips the full `read_to_string`
/// whenever the byte length alone proves the content changed (the common
/// case when a regenerated spec differs) — only an exact length match
/// falls back to the full read + compare.  Missing or unreadable files
/// count as "changed", so the caller writes — exactly like the previous
/// `read_to_string(...).map_or(true, |e| e != content)` this replaces.
pub(super) fn content_unchanged(path: &Path, content: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == content.len() as u64)
        && std::fs::read_to_string(path).is_ok_and(|existing| existing == content)
}

/// Generate `OpenAPI` JSON and write to files, returning docs info
pub fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
    file_asts: HashMap<String, syn::File>,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<OpenApiWriteResult> {
    if input.openapi_file_names.is_empty() && input.docs_url.is_none() && input.redoc_url.is_none()
    {
        return Ok(OpenApiWriteResult {
            docs_url: None,
            redoc_url: None,
            spec_json: None,
            spec_pretty: None,
        });
    }

    let mut openapi_doc = try_generate_openapi_doc_with_metadata(
        input.title.clone(),
        input.version.clone(),
        input.servers.clone(),
        Some(OpenApiSecurity {
            security_schemes: input.security_schemes.clone(),
            security: input.security.clone(),
            tag_descriptions: input.tag_descriptions.clone(),
        }),
        metadata,
        Some(file_asts),
        route_storage,
    )?;

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
                let spec_content = std::fs::read_to_string(&spec_file).map_err(|e| {
                    err_call_site(format!(
                        "OpenAPI merge: failed to read child spec for `{struct_name}` at '{}'. Error: {e}. Ensure the child crate containing `export_app!({struct_name})` is built before the parent app.",
                        spec_file.display()
                    ))
                })?;
                let child_spec = serde_json::from_str::<vespera_core::openapi::OpenApi>(
                    &spec_content,
                )
                .map_err(|e| {
                    err_call_site(format!(
                        "OpenAPI merge: failed to parse child spec for `{struct_name}` at '{}'. Error: {e}.",
                        spec_file.display()
                    ))
                })?;
                openapi_doc.merge(child_spec);
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
    let spec_pretty = if input.openapi_file_names.is_empty() {
        None
    } else {
        let json_pretty = serde_json::to_string_pretty(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?;
        for openapi_file_name in &input.openapi_file_names {
            let file_path = Path::new(openapi_file_name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err_call_site(format!("OpenAPI output: failed to create directory '{}'. Error: {}. Ensure the path is valid and writable.", parent.display(), e)))?;
            }
            let should_write = !content_unchanged(file_path, &json_pretty);
            if should_write {
                std::fs::write(file_path, &json_pretty).map_err(|e| err_call_site(format!("OpenAPI output: failed to write file '{openapi_file_name}'. Error: {e}. Ensure the file path is writable.")))?;
            }
        }
        Some(json_pretty)
    };

    // Compact JSON for embedding (smaller binary, faster downstream compilation).
    let spec_json = if input.docs_url.is_some() || input.redoc_url.is_some() {
        Some(serde_json::to_string(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?)
    } else {
        None
    };

    Ok(OpenApiWriteResult {
        docs_url: input.docs_url.clone(),
        redoc_url: input.redoc_url.clone(),
        spec_json,
        spec_pretty,
    })
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
        let should_write = !content_unchanged(file_path, pretty);
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

/// Path of the compact-spec embed sidecar (`include_str!` target).
///
/// The file name is **namespaced per crate**: two workspace members
/// both using `vespera!` compile in parallel under the same shared
/// `target/vespera/` directory — with a single shared file name, crate
/// A's `include_str!` could read the spec crate B just wrote.
pub(super) fn embed_spec_path() -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    find_target_dir(Path::new(&manifest_dir))
        .join("vespera")
        .join(format!("vespera_spec-{}.json", current_crate_tag()))
}

/// Path of the pretty-spec sidecar (warm-rebuild source for
/// `openapi.json` recovery — see `ensure_openapi_files_from_cache`).
pub(super) fn pretty_sidecar_path() -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    find_target_dir(Path::new(&manifest_dir))
        .join("vespera")
        .join(format!("openapi_pretty-{}.json", current_crate_tag()))
}

/// Build the `include_str!` tokens pointing at the embed sidecar.
fn embed_tokens(spec_file: &Path) -> proc_macro2::TokenStream {
    let path_str = crate::file_utils::path_to_include_str_literal(spec_file);
    quote::quote! { include_str!(#path_str) }
}

/// Hash-validated sidecar specs loaded on a warm cache hit.
pub(super) struct SidecarSpecs {
    /// Pretty spec content (for `openapi.json` recovery); `None` when
    /// no openapi file is configured.
    pub(super) pretty: Option<String>,
    /// `include_str!` tokens for the embed sidecar; `None` when docs
    /// are disabled.
    pub(super) spec_tokens: Option<proc_macro2::TokenStream>,
}

/// Load and hash-validate the sidecar spec files on a warm cache hit.
///
/// Returns `None` when any expected sidecar is missing or fails its
/// content-hash check — the caller must then treat the cache as a miss
/// (a full regeneration rewrites both sidecars, so corruption
/// self-heals on the next build).
pub(super) fn load_validated_sidecar_specs(
    spec_json_hash: Option<u64>,
    spec_pretty_hash: Option<u64>,
) -> Option<SidecarSpecs> {
    let spec_tokens = match spec_json_hash {
        None => None,
        Some(expected) => {
            let path = embed_spec_path();
            let content = std::fs::read_to_string(&path).ok()?;
            if super::cache::hash_str(&content) != expected {
                return None;
            }
            Some(embed_tokens(&path))
        }
    };
    let pretty = match spec_pretty_hash {
        None => None,
        Some(expected) => {
            let content = std::fs::read_to_string(pretty_sidecar_path()).ok()?;
            if super::cache::hash_str(&content) != expected {
                return None;
            }
            Some(content)
        }
    };
    Some(SidecarSpecs {
        pretty,
        spec_tokens,
    })
}

/// Write the pretty-spec sidecar (write-if-differs).  Best-effort like
/// the cache itself: failures only cost a future cache miss.
pub(super) fn write_pretty_sidecar(spec_pretty: Option<&str>) {
    let Some(pretty) = spec_pretty else {
        return;
    };
    let path = pretty_sidecar_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let should_write = !content_unchanged(&path, pretty);
    if should_write {
        let _ = std::fs::write(&path, pretty);
    }
}

/// Write compact spec JSON to target dir for `include_str!` embedding.
pub(super) fn write_spec_for_embedding(
    spec_json: Option<String>,
) -> syn::Result<Option<proc_macro2::TokenStream>> {
    let Some(json) = spec_json else {
        return Ok(None);
    };
    let spec_file = embed_spec_path();
    if let Some(parent) = spec_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            syn::Error::new(
                Span::call_site(),
                format!(
                    "vespera! macro: failed to create directory '{}': {}",
                    parent.display(),
                    e
                ),
            )
        })?;
    }
    let should_write = !content_unchanged(&spec_file, &json);
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
    Ok(Some(embed_tokens(&spec_file)))
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.docs_url.is_none());
        assert!(result.redoc_url.is_none());
        assert!(result.spec_json.is_none());
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
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.docs_url.is_some());
        assert_eq!(result.docs_url.unwrap(), "/docs");
        assert!(result.spec_json.is_some());
        let json = result.spec_json.unwrap();
        assert!(json.contains("\"openapi\""));
        assert!(json.contains("Test API"));
        assert!(result.redoc_url.is_none());
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
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.docs_url.is_none());
        assert!(result.redoc_url.is_some());
        assert_eq!(result.redoc_url.unwrap(), "/redoc");
        assert!(result.spec_json.is_some());
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
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.docs_url.is_some());
        assert!(result.redoc_url.is_some());
        assert!(result.spec_json.is_some());
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
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
