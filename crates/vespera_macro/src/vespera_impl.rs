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

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::Path,
};

use proc_macro2::Span;
use quote::quote;

use serde::{Deserialize, Serialize};

use crate::{
    collector::{collect_file_fingerprints, collect_metadata},
    error::{MacroResult, err_call_site},
    metadata::{CollectedMetadata, StructMetadata},
    openapi_generator::generate_openapi_doc_with_metadata,
    route_impl::StoredRouteInfo,
    router_codegen::{ProcessedVesperaInput, generate_router_code},
};

/// Docs info tuple type alias for cleaner signatures
pub type DocsInfo = (Option<String>, Option<String>, Option<String>);

/// Cache for avoiding redundant route scanning and OpenAPI generation.
/// Persisted to `target/vespera/routes.cache` across builds.
#[derive(Serialize, Deserialize)]
struct VesperaCache {
    /// Macro crate version — invalidates cache when macro code changes
    #[serde(default)]
    macro_version: String,
    /// File path → modification time (secs since UNIX_EPOCH)
    file_fingerprints: HashMap<String, u64>,
    /// Hash of SCHEMA_STORAGE contents
    schema_hash: u64,
    /// Hash of OpenAPI config (title, version, servers, docs_url, etc.)
    config_hash: u64,
    /// Cached route/struct metadata
    metadata: CollectedMetadata,
    /// Compact JSON for docs embedding (None if docs disabled)
    spec_json: Option<String>,
    /// Pretty JSON for file output (None if no openapi file configured)
    spec_pretty: Option<String>,
}

/// Compute a deterministic hash of SCHEMA_STORAGE contents.
fn compute_schema_hash(schema_storage: &HashMap<String, StructMetadata>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut keys: Vec<&String> = schema_storage.keys().collect();
    keys.sort();
    for key in keys {
        key.hash(&mut hasher);
        let meta = &schema_storage[key];
        meta.name.hash(&mut hasher);
        meta.definition.hash(&mut hasher);
        meta.include_in_openapi.hash(&mut hasher);
    }
    hasher.finish()
}

/// Compute a deterministic hash of OpenAPI config fields.
fn compute_config_hash(processed: &ProcessedVesperaInput) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    processed.title.hash(&mut hasher);
    processed.version.hash(&mut hasher);
    processed.docs_url.hash(&mut hasher);
    processed.redoc_url.hash(&mut hasher);
    processed.openapi_file_names.hash(&mut hasher);
    if let Some(ref servers) = processed.servers {
        for s in servers {
            s.url.hash(&mut hasher);
        }
    }
    for merge_path in &processed.merge {
        quote!(#merge_path).to_string().hash(&mut hasher);
    }
    hasher.finish()
}

/// Get the path to the routes cache file.
fn get_cache_path() -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    find_target_dir(manifest_path)
        .join("vespera")
        .join("routes.cache")
}

/// Try to read and deserialize a cache file. Returns None on any failure.
fn read_cache(cache_path: &Path) -> Option<VesperaCache> {
    let content = std::fs::read_to_string(cache_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write cache to disk. Failures are silently ignored (cache is best-effort).
fn write_cache(cache_path: &Path, cache: &VesperaCache) {
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(cache_path, json);
    }
}

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

    // Pretty-print for user-visible files
    if !input.openapi_file_names.is_empty() {
        let json_pretty = serde_json::to_string_pretty(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?;
        for openapi_file_name in &input.openapi_file_names {
            let file_path = Path::new(openapi_file_name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err_call_site(format!("OpenAPI output: failed to create directory '{}'. Error: {}. Ensure the path is valid and writable.", parent.display(), e)))?;
            }
            let should_write = std::fs::read_to_string(file_path)
                .map(|existing| existing != json_pretty)
                .unwrap_or(true);
            if should_write {
                std::fs::write(file_path, &json_pretty).map_err(|e| err_call_site(format!("OpenAPI output: failed to write file '{openapi_file_name}'. Error: {e}. Ensure the file path is writable.")))?;
            }
        }
    }

    // Compact JSON for embedding (smaller binary, faster downstream compilation)
    let spec_json = if input.docs_url.is_some() || input.redoc_url.is_some() {
        Some(serde_json::to_string(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?)
    } else {
        None
    };

    Ok((input.docs_url.clone(), input.redoc_url.clone(), spec_json))
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

/// Supplement collector's `RouteMetadata` with data from `ROUTE_STORAGE`.
///
/// `#[route]` stores metadata at attribute expansion time.
/// `collector.rs` re-parses the same data from file ASTs.
/// This function merges ROUTE_STORAGE data into collector's output,
/// preferring ROUTE_STORAGE values when they provide richer info.
///
/// Matching is by function name. If multiple routes share a function name,
/// the match is ambiguous and ROUTE_STORAGE data is skipped for safety.
fn merge_route_storage_data(metadata: &mut CollectedMetadata, route_storage: &[StoredRouteInfo]) {
    if route_storage.is_empty() {
        return;
    }

    for route in &mut metadata.routes {
        // Find matching StoredRouteInfo by function name
        let mut matches = route_storage
            .iter()
            .filter(|s| s.fn_name == route.function_name);

        let Some(stored) = matches.next() else {
            continue;
        };

        // Skip if ambiguous (multiple routes with same function name)
        if matches.next().is_some() {
            continue;
        }

        // Supplement with ROUTE_STORAGE data
        // Only override when ROUTE_STORAGE has an explicit value
        if let Some(ref tags) = stored.tags {
            route.tags = Some(tags.clone());
        }
        if let Some(ref desc) = stored.description {
            route.description = Some(desc.clone());
        }
        if let Some(ref status) = stored.error_status {
            route.error_status = Some(status.clone());
        }
    }
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
        let should_write = std::fs::read_to_string(file_path)
            .map(|existing| existing != *pretty)
            .unwrap_or(true);
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
fn write_spec_for_embedding(
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
    let spec_file = vespera_dir.join("vespera_spec.json");
    let should_write = std::fs::read_to_string(&spec_file)
        .map(|existing| existing != json)
        .unwrap_or(true);
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

/// Process vespera macro - extracted for testability
#[allow(clippy::too_many_lines)]
pub fn process_vespera_macro(
    processed: &ProcessedVesperaInput,
    schema_storage: &HashMap<String, StructMetadata>,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<proc_macro2::TokenStream> {
    let profile_start = if std::env::var("VESPERA_PROFILE").is_ok() {
        Some(std::time::Instant::now())
    } else {
        None
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
    let cache_path = get_cache_path();
    let fingerprints = collect_file_fingerprints(&folder_path)
        .map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: {e}")))?;
    let schema_hash = compute_schema_hash(schema_storage);
    let config_hash = compute_config_hash(processed);

    let macro_version = env!("CARGO_PKG_VERSION").to_string();
    let cached = read_cache(&cache_path);
    let cache_hit = cached.as_ref().is_some_and(|c| {
        c.macro_version == macro_version
            && c.file_fingerprints == fingerprints
            && c.schema_hash == schema_hash
            && c.config_hash == config_hash
    });

    let (metadata, spec_json) = if cache_hit {
        let cache = cached.unwrap();
        let mut metadata = cache.metadata;
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata
            .check_duplicate_schema_names()
            .map_err(|msg| syn::Error::new(Span::call_site(), format!("vespera! macro: {msg}")))?;

        // Ensure openapi.json files exist and are up-to-date from cache
        ensure_openapi_files_from_cache(
            &processed.openapi_file_names,
            cache.spec_pretty.as_deref(),
        )?;

        (metadata, cache.spec_json)
    } else {
        let (mut metadata, file_asts) = collect_metadata(&folder_path, &processed.folder_name, route_storage).map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: failed to scan route folder '{}'. Error: {}. Check that all .rs files have valid Rust syntax.", processed.folder_name, e)))?;

        // Clone metadata before extending (cache stores file-only structs)
        let cache_metadata = metadata.clone();
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata
            .check_duplicate_schema_names()
            .map_err(|msg| syn::Error::new(Span::call_site(), format!("vespera! macro: {msg}")))?;

        let (_, _, spec_json) =
            generate_and_write_openapi(processed, &metadata, file_asts, route_storage)?;

        // Read back spec_pretty from first openapi file for caching
        let spec_pretty = processed
            .openapi_file_names
            .first()
            .and_then(|f| std::fs::read_to_string(f).ok());

        // Persist cache (best-effort, failures are silent)
        write_cache(
            &cache_path,
            &VesperaCache {
                macro_version: macro_version.clone(),
                file_fingerprints: fingerprints,
                schema_hash,
                config_hash,
                metadata: cache_metadata,
                spec_json: spec_json.clone(),
                spec_pretty,
            },
        );

        (metadata, spec_json)
    };

    // Write compact spec for include_str! embedding
    let spec_tokens = write_spec_for_embedding(spec_json)?;

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

    // Generate OpenAPI spec JSON string
    let openapi_doc = generate_openapi_doc_with_metadata(
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
    use crate::metadata::RouteMetadata;

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
        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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

        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);

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

        let result = generate_and_write_openapi(&processed, &metadata, HashMap::new(), &[]);
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

    // ========== Tests for merge_route_storage_data ==========

    #[test]
    fn test_merge_route_storage_empty_storage() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: "pub async fn get_users() -> Json<Vec<User>>".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        merge_route_storage_data(&mut metadata, &[]);
        // No changes when storage is empty
        assert!(metadata.routes[0].tags.is_none());
        assert!(metadata.routes[0].description.is_none());
        assert!(metadata.routes[0].error_status.is_none());
    }

    #[test]
    fn test_merge_route_storage_matching_route() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: "pub async fn get_users() -> Json<Vec<User>>".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            tags: Some(vec!["users".to_string()]),
            description: Some("List all users".to_string()),
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        assert_eq!(metadata.routes[0].tags, Some(vec!["users".to_string()]));
        assert_eq!(
            metadata.routes[0].description,
            Some("List all users".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400, 404]));
    }

    #[test]
    fn test_merge_route_storage_no_match() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: String::new(),
            error_status: None,
            tags: None,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "create_user".to_string(),
            method: Some("post".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            tags: Some(vec!["users".to_string()]),
            description: None,
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // No match — fields unchanged
        assert!(metadata.routes[0].tags.is_none());
        assert!(metadata.routes[0].error_status.is_none());
    }

    #[test]
    fn test_merge_route_storage_ambiguous_skipped() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "handler".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: String::new(),
            error_status: None,
            tags: None,
            description: None,
        });

        // Two StoredRouteInfo with same fn_name — ambiguous
        let storage = vec![
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                tags: Some(vec!["file-a".to_string()]),
                description: None,
                fn_item_str: String::new(),
                file_path: None,
            },
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("post".to_string()),
                custom_path: None,
                error_status: None,
                tags: Some(vec!["file-b".to_string()]),
                description: None,
                fn_item_str: String::new(),
                file_path: None,
            },
        ];

        merge_route_storage_data(&mut metadata, &storage);
        // Ambiguous match — no merge
        assert!(metadata.routes[0].tags.is_none());
    }

    #[test]
    fn test_merge_route_storage_preserves_existing() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: String::new(),
            error_status: Some(vec![500]),
            tags: Some(vec!["existing-tag".to_string()]),
            description: Some("Existing description".to_string()),
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            tags: Some(vec!["new-tag".to_string()]),
            description: Some("New description".to_string()),
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // ROUTE_STORAGE values override when they have explicit values
        assert_eq!(metadata.routes[0].tags, Some(vec!["new-tag".to_string()]));
        assert_eq!(
            metadata.routes[0].description,
            Some("New description".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400, 404]));
    }

    #[test]
    fn test_merge_route_storage_partial_fields() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            signature: String::new(),
            error_status: None,
            tags: Some(vec!["from-collector".to_string()]),
            description: Some("From doc comment".to_string()),
        });

        // StoredRouteInfo with only error_status (tags/description are None)
        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            tags: None,
            description: None,
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // Only error_status should be set; tags and description preserved from collector
        assert_eq!(
            metadata.routes[0].tags,
            Some(vec!["from-collector".to_string()])
        );
        assert_eq!(
            metadata.routes[0].description,
            Some("From doc comment".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400]));
    }

    #[test]
    fn test_compute_config_hash_with_servers() {
        // Exercises lines 92-96: servers loop in compute_config_hash
        let processed_no_servers = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };

        let processed_with_servers = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
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
            merge: vec![],
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
        // Exercises lines 97-99: merge loop in compute_config_hash
        let processed_no_merge = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };

        let processed_with_merge = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![syn::parse_quote!(app::TestApp)],
        };

        let hash_no_merge = compute_config_hash(&processed_no_merge);
        let hash_with_merge = compute_config_hash(&processed_with_merge);

        assert_ne!(
            hash_no_merge, hash_with_merge,
            "Merge paths should affect config hash"
        );
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
