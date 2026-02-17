//! Core implementation of vespera! and `export_app`! macros.
//!
//! This module orchestrates the entire macro execution flow:
//! - Route discovery via filesystem scanning
//! - `OpenAPI` spec generation
//! - File I/O for writing `OpenAPI` JSON
//! - Router code generation
//!
//! # Overview
//!
//! This is the main orchestrator for the two primary macros:
//! - `vespera!()` - Generates a complete Axum router with `OpenAPI` spec
//! - `export_app!()` - Exports a router for merging into parent apps
//!
//! The execution flow is:
//! 1. Parse macro arguments via [`router_codegen`]
//! 2. Discover routes via [`collector::collect_metadata`]
//! 3. Generate `OpenAPI` spec via [`openapi_generator`]
//! 4. Write `OpenAPI` JSON files (if configured)
//! 5. Generate router code via [`router_codegen::generate_router_code`]
//!
//! # Key Functions
//!
//! - [`process_vespera_macro`] - Main vespera! macro implementation
//! - [`process_export_app`] - Main `export_app`! macro implementation
//! - [`generate_and_write_openapi`] - `OpenAPI` generation and file I/O

use std::{collections::HashMap, path::Path};

use proc_macro2::Span;
use quote::quote;

use crate::{
    collector::collect_metadata,
    error::{MacroResult, err_call_site},
    metadata::{CollectedMetadata, StructMetadata},
    openapi_generator::generate_openapi_doc_with_metadata,
    router_codegen::{ProcessedVesperaInput, generate_router_code},
};

/// Docs info tuple type alias for cleaner signatures
pub type DocsInfo = (Option<(String, String)>, Option<(String, String)>);

/// Generate `OpenAPI` JSON and write to files, returning docs info
pub fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
) -> MacroResult<DocsInfo> {
    if input.openapi_file_names.is_empty() && input.docs_url.is_none() && input.redoc_url.is_none()
    {
        return Ok((None, None));
    }

    let mut openapi_doc = generate_openapi_doc_with_metadata(
        input.title.clone(),
        input.version.clone(),
        input.servers.clone(),
        metadata,
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

    let json_str = serde_json::to_string_pretty(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?;

    for openapi_file_name in &input.openapi_file_names {
        let file_path = Path::new(openapi_file_name);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err_call_site(format!("OpenAPI output: failed to create directory '{}'. Error: {}. Ensure the path is valid and writable.", parent.display(), e)))?;
        }
        std::fs::write(file_path, &json_str).map_err(|e| err_call_site(format!("OpenAPI output: failed to write file '{openapi_file_name}'. Error: {e}. Ensure the file path is writable.")))?;
    }

    let docs_info = input
        .docs_url
        .as_ref()
        .map(|url| (url.clone(), json_str.clone()));
    let redoc_info = input.redoc_url.as_ref().map(|url| (url.clone(), json_str));

    Ok((docs_info, redoc_info))
}

/// Find the folder path for route scanning
pub fn find_folder_path(folder_name: &str) -> MacroResult<std::path::PathBuf> {
    let root = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        err_call_site(
            "CARGO_MANIFEST_DIR is not set. vespera macros must be used within a cargo build.",
        )
    })?;
    let path = format!("{root}/src/{folder_name}");
    let path = Path::new(&path);
    if path.exists() && path.is_dir() {
        return Ok(path.to_path_buf());
    }

    Ok(Path::new(folder_name).to_path_buf())
}

/// Find the workspace root's target directory
pub fn find_target_dir(manifest_path: &Path) -> std::path::PathBuf {
    // Look for workspace root by finding a Cargo.toml with [workspace] section
    let mut current = Some(manifest_path);
    let mut last_with_lock = None;

    while let Some(dir) = current {
        // Check if this directory has Cargo.lock
        if dir.join("Cargo.lock").exists() {
            last_with_lock = Some(dir.to_path_buf());
        }

        // Check if this is a workspace root (has Cargo.toml with [workspace])
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists()
            && let Ok(contents) = std::fs::read_to_string(&cargo_toml)
            && contents.contains("[workspace]")
        {
            return dir.join("target");
        }

        current = dir.parent();
    }

    // If we found a Cargo.lock but no [workspace], use the topmost one
    if let Some(lock_dir) = last_with_lock {
        return lock_dir.join("target");
    }

    // Fallback: use manifest dir's target
    manifest_path.join("target")
}

/// Process vespera macro - extracted for testability
pub fn process_vespera_macro(
    processed: &ProcessedVesperaInput,
    schema_storage: &HashMap<String, StructMetadata>,
) -> syn::Result<proc_macro2::TokenStream> {
    let folder_path = find_folder_path(&processed.folder_name)?;
    if !folder_path.exists() {
        return Err(syn::Error::new(
            Span::call_site(),
            format!(
                "vespera! macro: route folder '{}' not found. Create src/{} or specify a different folder with `dir = \"your_folder\"`.",
                processed.folder_name, processed.folder_name
            ),
        ));
    }

    let mut metadata = collect_metadata(&folder_path, &processed.folder_name).map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: failed to scan route folder '{}'. Error: {}. Check that all .rs files have valid Rust syntax.", processed.folder_name, e)))?;
    metadata.structs.extend(schema_storage.values().cloned());

    let (docs_info, redoc_info) = generate_and_write_openapi(processed, &metadata)?;

    Ok(generate_router_code(
        &metadata,
        docs_info,
        redoc_info,
        &processed.merge,
    ))
}

/// Process `export_app` macro - extracted for testability
pub fn process_export_app(
    name: &syn::Ident,
    folder_name: &str,
    schema_storage: &HashMap<String, StructMetadata>,
    manifest_dir: &str,
) -> syn::Result<proc_macro2::TokenStream> {
    let folder_path = find_folder_path(folder_name)?;
    if !folder_path.exists() {
        return Err(syn::Error::new(
            Span::call_site(),
            format!(
                "export_app! macro: route folder '{folder_name}' not found. Create src/{folder_name} or specify a different folder with `dir = \"your_folder\"`."
            ),
        ));
    }

    let mut metadata = collect_metadata(&folder_path, folder_name).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to scan route folder '{folder_name}'. Error: {e}. Check that all .rs files have valid Rust syntax.")))?;
    metadata.structs.extend(schema_storage.values().cloned());

    // Generate OpenAPI spec JSON string
    let openapi_doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);
    let spec_json = serde_json::to_string(&openapi_doc).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to serialize OpenAPI spec to JSON. Error: {e}. Check that all schema types are serializable.")))?;

    // Write spec to temp file for compile-time merging by parent apps
    let name_str = name.to_string();
    let manifest_path = Path::new(manifest_dir);
    let target_dir = find_target_dir(manifest_path);
    let vespera_dir = target_dir.join("vespera");
    std::fs::create_dir_all(&vespera_dir).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to create build cache directory '{}'. Error: {}. Ensure the target directory is writable.", vespera_dir.display(), e)))?;
    let spec_file = vespera_dir.join(format!("{name_str}.openapi.json"));
    std::fs::write(&spec_file, &spec_json).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to write OpenAPI spec file '{}'. Error: {}. Ensure the file path is writable.", spec_file.display(), e)))?;

    // Generate router code (without docs routes, no merge)
    let router_code = generate_router_code(&metadata, None, None, &[]);

    Ok(quote! {
        /// Auto-generated vespera app struct
        pub struct #name;

        impl #name {
            /// OpenAPI specification as JSON string
            pub const OPENAPI_SPEC: &'static str = #spec_json;

            /// Create the router for this app.
            /// Returns `Router<()>` which can be merged into any other router.
            pub fn router() -> vespera::axum::Router<()> {
                #router_code
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    // ========== Tests for generate_and_write_openapi ==========

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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_none());
        assert!(redoc_info.is_none());
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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_some());
        let (url, json) = docs_info.unwrap();
        assert_eq!(url, "/docs");
        assert!(json.contains("\"openapi\""));
        assert!(json.contains("Test API"));
        assert!(redoc_info.is_none());
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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_none());
        assert!(redoc_info.is_some());
        let (url, _) = redoc_info.unwrap();
        assert_eq!(url, "/redoc");
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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_some());
        assert!(redoc_info.is_some());
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
        let result = generate_and_write_openapi(&processed, &metadata);
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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());

        // Verify nested directories and file were created
        assert!(output_path.exists());
    }

    // ========== Tests for find_folder_path ==========
    // Note: find_folder_path uses CARGO_MANIFEST_DIR which is set during cargo test

    #[test]
    fn test_find_folder_path_nonexistent_returns_path() {
        // When the constructed path doesn't exist, it falls back to using folder_name directly
        let result = find_folder_path("nonexistent_folder_xyz").unwrap();
        // It should return a PathBuf (either from src/nonexistent... or just the folder name)
        assert!(result.to_string_lossy().contains("nonexistent_folder_xyz"));
    }

    // ========== Tests for find_target_dir ==========

    #[test]
    fn test_find_target_dir_no_workspace() {
        // Test fallback to manifest dir's target
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();
        let result = find_target_dir(manifest_path);
        assert_eq!(result, manifest_path.join("target"));
    }

    #[test]
    fn test_find_target_dir_with_cargo_lock() {
        // Test finding target dir with Cargo.lock present
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();

        // Create Cargo.lock (but no [workspace] in Cargo.toml)
        fs::write(manifest_path.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        let result = find_target_dir(manifest_path);
        // Should use the directory with Cargo.lock
        assert_eq!(result, manifest_path.join("target"));
    }

    #[test]
    fn test_find_target_dir_with_workspace() {
        // Test finding workspace root
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create a workspace Cargo.toml
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate1\"]",
        )
        .expect("Failed to write Cargo.toml");

        // Create nested crate directory
        let crate_dir = workspace_root.join("crate1");
        fs::create_dir(&crate_dir).expect("Failed to create crate dir");
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"crate1\"")
            .expect("Failed to write Cargo.toml");

        let result = find_target_dir(&crate_dir);
        // Should return workspace root's target
        assert_eq!(result, workspace_root.join("target"));
    }

    #[test]
    fn test_find_target_dir_workspace_with_cargo_lock() {
        // Test that [workspace] takes priority over Cargo.lock
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create workspace Cargo.toml and Cargo.lock
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate1\"]",
        )
        .expect("Failed to write Cargo.toml");
        fs::write(workspace_root.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        // Create nested crate
        let crate_dir = workspace_root.join("crate1");
        fs::create_dir(&crate_dir).expect("Failed to create crate dir");
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"crate1\"")
            .expect("Failed to write Cargo.toml");

        let result = find_target_dir(&crate_dir);
        assert_eq!(result, workspace_root.join("target"));
    }

    #[test]
    fn test_find_target_dir_deeply_nested() {
        // Test deeply nested crate structure
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create workspace
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .expect("Failed to write Cargo.toml");

        // Create deeply nested crate
        let deep_crate = workspace_root.join("crates/group/my-crate");
        fs::create_dir_all(&deep_crate).expect("Failed to create nested dirs");
        fs::write(deep_crate.join("Cargo.toml"), "[package]").expect("Failed to write Cargo.toml");

        let result = find_target_dir(&deep_crate);
        assert_eq!(result, workspace_root.join("target"));
    }

    // ========== Tests for process_vespera_macro ==========

    #[test]
    fn test_process_vespera_macro_folder_not_found() {
        let processed = ProcessedVesperaInput {
            folder_name: "nonexistent_folder_xyz_123".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };
        let result = process_vespera_macro(&processed, &HashMap::new());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("route folder") && err.contains("not found"));
    }

    #[test]
    fn test_process_vespera_macro_collect_metadata_error() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an invalid route file (will cause parse error but collect_metadata handles it)
        create_temp_file(&temp_dir, "invalid.rs", "not valid rust code {{{");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: Some("Test API".to_string()),
            version: Some("1.0.0".to_string()),
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };

        // This exercises the collect_metadata path (which handles parse errors gracefully)
        let result = process_vespera_macro(&processed, &HashMap::new());
        // Result may succeed or fail depending on how collect_metadata handles invalid files
        let _ = result;
    }

    #[test]
    fn test_process_vespera_macro_with_schema_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty file (valid but no routes)
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        let schema_storage = HashMap::from([(
            "TestSchema".to_string(),
            StructMetadata::new(
                "TestSchema".to_string(),
                "struct TestSchema { id: i32 }".to_string(),
            ),
        )]);

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: Some("/docs".to_string()),
            redoc_url: Some("/redoc".to_string()),
            servers: None,
            merge: vec![],
        };

        // This exercises the schema_storage extend path
        let result = process_vespera_macro(&processed, &schema_storage);
        // We only care about exercising the code path
        let _ = result;
    }

    // ========== Tests for process_export_app ==========

    #[test]
    fn test_process_export_app_folder_not_found() {
        let name: syn::Ident = syn::parse_quote!(TestApp);
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let result = process_export_app(
            &name,
            "nonexistent_folder_xyz",
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("route folder") && err.contains("not found"));
    }

    #[test]
    fn test_process_export_app_with_empty_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty file
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        // This exercises collect_metadata and other paths
        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
        );
        // We only care about exercising the code path
        let _ = result;
    }

    #[test]
    fn test_process_export_app_with_schema_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty but valid Rust file
        create_temp_file(&temp_dir, "mod.rs", "// module file\n");

        let schema_storage = HashMap::from([(
            "AppSchema".to_string(),
            StructMetadata::new(
                "AppSchema".to_string(),
                "struct AppSchema { name: String }".to_string(),
            ),
        )]);

        let name: syn::Ident = syn::parse_quote!(MyExportedApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &schema_storage,
            &temp_dir.path().to_string_lossy(),
        );
        // Exercises the schema_storage.extend path
        let _ = result;
    }

    // ========== Tests for generate_and_write_openapi with merge ==========

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
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
    }

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

        let result = generate_and_write_openapi(&processed, &metadata);

        // Restore CARGO_MANIFEST_DIR
        if let Some(old_value) = old_manifest_dir {
            // SAFETY: We're in a single-threaded test context
            unsafe { std::env::set_var("CARGO_MANIFEST_DIR", old_value) };
        }

        assert!(result.is_ok());
    }

    // ========== Tests for find_folder_path ==========

    #[test]
    fn test_find_folder_path_absolute_path() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let absolute_path = temp_dir.path().to_string_lossy().to_string();

        // When given an absolute path that exists, it should return it
        let result = find_folder_path(&absolute_path).unwrap();
        // The function tries src/{folder_name} first, then falls back to the folder_name directly
        assert!(
            result.to_string_lossy().contains(&absolute_path)
                || result == Path::new(&absolute_path)
        );
    }

    #[test]
    fn test_find_folder_path_with_src_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create src/routes directory
        let src_routes = temp_dir.path().join("src").join("routes");
        fs::create_dir_all(&src_routes).expect("Failed to create src/routes dir");

        // Save and set CARGO_MANIFEST_DIR
        let old_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: We're in a single-threaded test context
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result = find_folder_path("routes").unwrap();

        // Restore CARGO_MANIFEST_DIR
        if let Some(old_value) = old_manifest_dir {
            // SAFETY: We're in a single-threaded test context
            unsafe { std::env::set_var("CARGO_MANIFEST_DIR", old_value) };
        }

        // Should return the src/routes path since it exists
        assert!(
            result.to_string_lossy().contains("src") && result.to_string_lossy().contains("routes")
        );
    }

    // ========== Error path coverage tests ==========

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

        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to write file"));
    }

    #[test]
    fn test_process_export_app_collect_metadata_error() {
        // Lines 210-212: collect_metadata returns error for invalid Rust syntax
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create a file with invalid Rust syntax that will cause parse error
        create_temp_file(&temp_dir, "invalid.rs", "fn broken( { syntax error");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to scan route folder"));
    }

    #[test]
    fn test_process_export_app_create_dir_error() {
        // Lines 232-234: create_dir_all failure when path contains a file
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty valid Rust file
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        // Create target directory but make 'vespera' a file instead of directory
        let target_dir = temp_dir.path().join("target");
        fs::create_dir(&target_dir).expect("Failed to create target dir");
        fs::write(target_dir.join("vespera"), "blocking file").expect("Failed to write file");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to create build cache directory"));
    }

    #[test]
    fn test_process_export_app_write_spec_error() {
        // Lines 239-241: fs::write failure when spec file path is a directory
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty valid Rust file
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        // Create target/vespera directory and make spec file name a directory
        let vespera_dir = temp_dir.path().join("target").join("vespera");
        fs::create_dir_all(&vespera_dir).expect("Failed to create vespera dir");
        // Create a directory where the spec file should be written
        fs::create_dir(vespera_dir.join("TestApp.openapi.json"))
            .expect("Failed to create blocking dir");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to write OpenAPI spec file"));
    }
}
