use std::{collections::HashMap, path::Path};

use proc_macro2::Span;
use quote::quote;

use crate::{
    collector::collect_metadata,
    metadata::StructMetadata,
    openapi_generator::generate_openapi_doc_with_metadata,
    route_impl::StoredRouteInfo,
    router_codegen::{ProcessedVesperaInput, generate_router_code},
};

use super::{
    cache::{
        CACHE_FORMAT, VesperaCache, compute_config_hash, compute_macro_dev_fingerprint,
        compute_schema_hash, get_cache_path, hash_str, read_cache, write_cache,
    },
    openapi_io::{
        ensure_openapi_files_from_cache, generate_and_write_openapi, load_validated_sidecar_specs,
        write_pretty_sidecar, write_spec_for_embedding,
    },
    path_utils::{find_folder_path, find_target_dir},
    route_merge::merge_route_storage_data,
};

/// Process vespera macro - extracted for testability
#[allow(clippy::too_many_lines)]
pub fn process_vespera_macro(
    processed: &ProcessedVesperaInput,
    schema_storage: &HashMap<String, StructMetadata>,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<proc_macro2::TokenStream> {
    let profile_start = if std::env::var("VESPERA_PROFILE").is_ok() {
        eprintln!(
            "[vespera-profile] storage at expansion: {} routes, {} schemas",
            route_storage.len(),
            schema_storage.len()
        );
        Some(std::time::Instant::now())
    } else {
        None
    };

    // Stage timer for `VESPERA_PROFILE=1` — prints per-stage elapsed
    // times so regressions can be attributed (scan vs openapi vs
    // serialization vs codegen).
    let mut stage_start = std::time::Instant::now();
    let mut stage = |name: &str| {
        if profile_start.is_some() {
            eprintln!("[vespera-profile]   {name}: {:?}", stage_start.elapsed());
            stage_start = std::time::Instant::now();
        }
    };

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

    // --- Incremental cache check ---
    // One directory walk serves both the fingerprint map and (on a
    // cache miss) route collection below.
    let cache_path = get_cache_path();
    let scanned = crate::collector::scan_route_folder(&folder_path)
        .map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: {e}")))?;
    let fingerprints = crate::collector::fingerprints_from_scan(&scanned);
    let schema_hash = compute_schema_hash(schema_storage);
    let config_hash = compute_config_hash(processed);
    stage("fingerprints + hashes");

    let macro_version = env!("CARGO_PKG_VERSION").to_string();
    let macro_dev_fingerprint = compute_macro_dev_fingerprint();
    stage("macro_dev_fingerprint");
    let cached = read_cache(&cache_path);
    stage("read_cache");
    let cache_hit = cached.as_ref().is_some_and(|c| {
        c.cache_format == CACHE_FORMAT
            && c.macro_version == macro_version
            && c.macro_dev_fingerprint == macro_dev_fingerprint
            && c.file_fingerprints == fingerprints
            && c.schema_hash == schema_hash
            && c.config_hash == config_hash
    });
    // Hash-validate the sidecar spec files (the cache only stores
    // hashes — content lives in `target/vespera/`).  Validation
    // failure downgrades to a full regeneration, which rewrites the
    // sidecars: corruption self-heals on the next build.
    let sidecars = if cache_hit {
        let c = cached.as_ref().unwrap();
        load_validated_sidecar_specs(c.spec_json_hash, c.spec_pretty_hash)
    } else {
        None
    };
    stage("validate_sidecar_specs");

    let (metadata, spec_tokens) = if let Some(sidecars) = sidecars {
        let cache = cached.unwrap();
        let mut metadata = cache.metadata;
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata
            .check_duplicate_schema_names()
            .map_err(|msg| syn::Error::new(Span::call_site(), format!("vespera! macro: {msg}")))?;
        stage("cache_branch_metadata_merge");

        // Ensure openapi.json files exist and are up-to-date from cache
        ensure_openapi_files_from_cache(&processed.openapi_file_names, sidecars.pretty.as_deref())?;
        stage("ensure_openapi_files_from_cache");

        (metadata, sidecars.spec_tokens)
    } else {
        let scanned_files: Vec<std::path::PathBuf> =
            scanned.iter().map(|(path, _)| path.clone()).collect();
        let (mut metadata, file_asts) = crate::collector::collect_metadata_from_files(&scanned_files, &folder_path, &processed.folder_name, route_storage).map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: failed to scan route folder '{}'. Error: {}. Check that all .rs files have valid Rust syntax.", processed.folder_name, e)))?;
        stage("collect_metadata");

        // Clone metadata before extending (cache stores file-only structs)
        let cache_metadata = metadata.clone();
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata
            .check_duplicate_schema_names()
            .map_err(|msg| syn::Error::new(Span::call_site(), format!("vespera! macro: {msg}")))?;
        stage("metadata merge");

        // B2: reject same-file extractor structs that lack `#[derive(Schema)]`
        // before they silently vanish from the generated spec. Runs only here
        // (cache miss) — a cache hit is byte-identical source that already
        // passed, so the check would be redundant.
        crate::parser::validate_schema_backed_extractors(&metadata)?;
        stage("validate_schema_backed_extractors");

        let (_, _, spec_json) =
            generate_and_write_openapi(processed, &metadata, file_asts, route_storage)?;
        stage("generate_and_write_openapi");

        // Read back spec_pretty from first openapi file for the pretty
        // sidecar (warm-rebuild recovery source for openapi.json)
        let spec_pretty = processed
            .openapi_file_names
            .first()
            .and_then(|f| std::fs::read_to_string(f).ok());
        write_pretty_sidecar(spec_pretty.as_deref());

        // Persist cache (best-effort, failures are silent) — spec
        // contents live in the sidecar files; only hashes are cached.
        write_cache(
            &cache_path,
            &VesperaCache {
                cache_format: CACHE_FORMAT,
                macro_version: macro_version.clone(),
                macro_dev_fingerprint,
                file_fingerprints: fingerprints,
                schema_hash,
                config_hash,
                metadata: cache_metadata,
                spec_json_hash: spec_json.as_deref().map(hash_str),
                spec_pretty_hash: spec_pretty.as_deref().map(hash_str),
            },
        );
        stage("write_cache");

        // Write compact spec for include_str! embedding
        let spec_tokens = write_spec_for_embedding(spec_json)?;
        stage("write_spec_for_embedding");

        (metadata, spec_tokens)
    };

    // --- Cron job discovery from CRON_STORAGE ---
    // #[cron("...")] attribute already registers metadata at expansion time.
    // No folder scanning needed — just read the storage.
    let cron_jobs: Vec<crate::metadata::CronMetadata> = {
        let storage = crate::CRON_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let src_dir = std::env::var("CARGO_MANIFEST_DIR")
            .map(|d| {
                let p = std::path::PathBuf::from(d).join("src");
                // Canonicalize for reliable prefix stripping
                let canonical = p.canonicalize().unwrap_or(p);
                canonical.display().to_string().replace('\\', "/")
            })
            .unwrap_or_default();
        storage
            .iter()
            .map(|s| {
                // Derive module path from file_path relative to src/
                let module_path = s
                    .file_path
                    .as_ref()
                    .map(|fp| {
                        let canonical = std::path::Path::new(fp)
                            .canonicalize()
                            .map_or_else(|_| fp.clone(), |p| p.display().to_string());
                        let normalized = canonical.replace('\\', "/");
                        let relative = normalized
                            .strip_prefix(&src_dir)
                            .map_or(&*normalized, |rest| rest.trim_start_matches('/'));
                        // Convert path to module path: strip .rs, replace / with ::, strip mod
                        // Replace hyphens with underscores (Rust module convention)
                        relative
                            .trim_end_matches(".rs")
                            .replace('/', "::")
                            .replace('-', "_")
                            .trim_end_matches("::mod")
                            .to_string()
                    })
                    .unwrap_or_default();
                crate::metadata::CronMetadata {
                    expression: s.expression.clone(),
                    function_name: s.fn_name.clone(),
                    module_path,
                    file_path: s.file_path.clone().unwrap_or_default(),
                }
            })
            .collect()
    };

    let result = Ok(generate_router_code(
        &metadata,
        processed.docs_url.as_deref(),
        processed.redoc_url.as_deref(),
        spec_tokens,
        &processed.merge,
        &cron_jobs,
    ));
    stage("generate_router_code");

    if let Some(start) = profile_start {
        eprintln!(
            "[vespera-profile] vespera! macro total: {:?}",
            start.elapsed()
        );
        crate::schema_macro::print_profile_summary();
    }

    result
}

/// Process `export_app` macro - extracted for testability
pub fn process_export_app(
    name: &syn::Ident,
    folder_name: &str,
    schema_storage: &HashMap<String, StructMetadata>,
    manifest_dir: &str,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<proc_macro2::TokenStream> {
    let profile_start = if std::env::var("VESPERA_PROFILE").is_ok() {
        Some(std::time::Instant::now())
    } else {
        None
    };

    let folder_path = find_folder_path(folder_name)?;
    if !folder_path.exists() {
        return Err(syn::Error::new(
            Span::call_site(),
            format!(
                "export_app! macro: route folder '{folder_name}' not found. Create src/{folder_name} or specify a different folder with `dir = \"your_folder\"`.",
            ),
        ));
    }

    let (mut metadata, file_asts) = collect_metadata(&folder_path, folder_name, route_storage).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to scan route folder '{folder_name}'. Error: {e}. Check that all .rs files have valid Rust syntax.")))?;
    metadata.structs.extend(schema_storage.values().cloned());
    merge_route_storage_data(&mut metadata, route_storage);
    metadata
        .check_duplicate_schema_names()
        .map_err(|msg| syn::Error::new(Span::call_site(), format!("export_app! macro: {msg}")))?;

    // B2: same-file extractor structs without `#[derive(Schema)]` would be
    // silently dropped from the spec — reject them at compile time.
    crate::parser::validate_schema_backed_extractors(&metadata)?;

    // Generate OpenAPI spec JSON string
    let openapi_doc = generate_openapi_doc_with_metadata(
        None,
        None,
        None,
        None,
        &metadata,
        Some(file_asts),
        route_storage,
    );
    let spec_json = serde_json::to_string(&openapi_doc).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to serialize OpenAPI spec to JSON. Error: {e}. Check that all schema types are serializable.")))?;

    // Write spec to temp file for compile-time merging by parent apps
    let name_str = name.to_string();
    let manifest_path = Path::new(manifest_dir);
    let target_dir = find_target_dir(manifest_path);
    let vespera_dir = target_dir.join("vespera");
    std::fs::create_dir_all(&vespera_dir).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to create build cache directory '{}'. Error: {}. Ensure the target directory is writable.", vespera_dir.display(), e)))?;
    let spec_file = vespera_dir.join(format!("{name_str}.openapi.json"));
    std::fs::write(&spec_file, &spec_json).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to write OpenAPI spec file '{}'. Error: {}. Ensure the file path is writable.", spec_file.display(), e)))?;
    let spec_path_str = spec_file.display().to_string().replace('\\', "/");

    // Generate router code (without docs routes, no merge)
    let router_code = generate_router_code(&metadata, None, None, None, &[], &[]);

    let result = Ok(quote! {
        /// Auto-generated vespera app struct
        pub struct #name;

        impl #name {
            /// OpenAPI specification as JSON string
            pub const OPENAPI_SPEC: &'static str = include_str!(#spec_path_str);

            /// Create the router for this app.
            /// Returns `Router<()>` which can be merged into any other router.
            pub fn router() -> vespera::axum::Router<()> {
                #router_code
            }
        }
    });

    if let Some(start) = profile_start {
        eprintln!(
            "[vespera-profile] export_app! macro total: {:?}",
            start.elapsed()
        );
        crate::schema_macro::print_profile_summary();
    }

    result
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };
        let result = process_vespera_macro(&processed, &HashMap::new(), &[]);
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // This exercises the collect_metadata path (which handles parse errors gracefully)
        let result = process_vespera_macro(&processed, &HashMap::new(), &[]);
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
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // This exercises the schema_storage extend path
        let result = process_vespera_macro(&processed, &schema_storage, &[]);
        // We only care about exercising the code path
        let _ = result;
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_with_cron_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create src/ subfolder structure to simulate a real project
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(src_dir.join("routes")).expect("create routes dir");
        std::fs::write(src_dir.join("routes").join("health.rs"), "// empty\n")
            .expect("write health.rs");

        // Set CARGO_MANIFEST_DIR so module path derivation works
        let old_manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe {
            std::env::set_var(
                "CARGO_MANIFEST_DIR",
                temp_dir.path().to_string_lossy().as_ref(),
            );
        }

        // Populate CRON_STORAGE with a fake cron entry
        {
            let mut storage = crate::CRON_STORAGE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            storage.push(crate::cron_impl::StoredCronInfo {
                fn_name: "test_cron_job".to_string(),
                expression: "0 */5 * * * *".to_string(),
                file_path: Some(
                    src_dir
                        .join("routes")
                        .join("health.rs")
                        .display()
                        .to_string(),
                ),
            });
        }

        let processed = ProcessedVesperaInput {
            folder_name: src_dir.join("routes").to_string_lossy().to_string(),
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

        // This exercises the CRON_STORAGE → CronMetadata derivation path
        let result = process_vespera_macro(&processed, &HashMap::new(), &[]);
        assert!(
            result.is_ok(),
            "Should succeed with cron storage: {result:?}"
        );

        // Clean up CRON_STORAGE
        {
            let mut storage = crate::CRON_STORAGE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            storage.retain(|s| s.fn_name != "test_cron_job");
        }

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(val) = old_manifest {
                std::env::set_var("CARGO_MANIFEST_DIR", val);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
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
            &[],
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
            &[],
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
            &[],
        );
        // Exercises the schema_storage.extend path
        let _ = result;
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
            &[],
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
            &[],
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
            &[],
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to write OpenAPI spec file"));
    }
    #[test]
    fn test_process_vespera_macro_no_openapi_output() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty route file\n");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
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

        let result = process_vespera_macro(&processed, &HashMap::new(), &[]);
        assert!(
            result.is_ok(),
            "Should succeed with no openapi output configured"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_with_profiling() {
        let old_profile = std::env::var("VESPERA_PROFILE").ok();
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
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

        let result = process_vespera_macro(&processed, &HashMap::new(), &[]);

        // Restore
        unsafe {
            if let Some(val) = old_profile {
                std::env::set_var("VESPERA_PROFILE", val);
            } else {
                std::env::remove_var("VESPERA_PROFILE");
            }
        };

        assert!(result.is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn test_process_export_app_with_profiling() {
        let old_profile = std::env::var("VESPERA_PROFILE").ok();
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let name: syn::Ident = syn::parse_quote!(TestProfileApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
        );

        // Restore
        unsafe {
            if let Some(val) = old_profile {
                std::env::set_var("VESPERA_PROFILE", val);
            } else {
                std::env::remove_var("VESPERA_PROFILE");
            }
        };

        // Exercise the code path
        let _ = result;
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_cache_hit() {
        // Exercises lines 320-324, 327, 329: the cache_hit branch in process_vespera_macro.
        // First call populates the cache, second call hits it.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(
            &temp_dir,
            "users.rs",
            "pub async fn list_users() -> String { \"users\".to_string() }\n",
        );

        let folder_path = temp_dir.path().to_string_lossy().to_string();
        let openapi_path = temp_dir.path().join("openapi.json");

        // Set CARGO_MANIFEST_DIR so cache path resolves to temp_dir/target/vespera/
        let old_manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let processed = ProcessedVesperaInput {
            folder_name: folder_path.clone(),
            openapi_file_names: vec![openapi_path.to_string_lossy().to_string()],
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

        // First call: cache MISS — scans files, generates spec, writes cache
        let result1 = process_vespera_macro(&processed, &HashMap::new(), &[]);
        assert!(
            result1.is_ok(),
            "First call (cache miss) should succeed: {:?}",
            result1.err()
        );
        assert!(
            openapi_path.exists(),
            "openapi.json should be written on first call"
        );

        // Second call: cache HIT — exercises lines 320-324, 327, 329
        let result2 = process_vespera_macro(&processed, &HashMap::new(), &[]);
        assert!(
            result2.is_ok(),
            "Second call (cache hit) should succeed: {:?}",
            result2.err()
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(val) = old_manifest {
                std::env::set_var("CARGO_MANIFEST_DIR", val);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        };
    }
}
