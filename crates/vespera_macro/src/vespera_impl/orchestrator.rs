use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use proc_macro2::Span;
use quote::quote;

use crate::{
    metadata::StructMetadata,
    route_impl::StoredRouteInfo,
    router_codegen::{ProcessedVesperaInput, generate_router_code},
};

use super::{
    cache::{
        CACHE_FORMAT, MergeSpecCache, VesperaCache, compute_config_hash_with_merge_cache,
        compute_export_config_hash, compute_macro_dev_fingerprint, compute_schema_hash,
        get_cache_path, get_export_cache_path, hash_str, read_cache, sidecar_matches, write_cache,
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
    folder_span: Span,
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
            folder_span,
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
    let mut merge_specs = MergeSpecCache::new();
    let config_hash = compute_config_hash_with_merge_cache(processed, &mut merge_specs);
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
        load_validated_sidecar_specs(
            c.spec_json_hash,
            c.spec_pretty_hash,
            c.spec_json_fingerprint,
            c.spec_pretty_fingerprint,
        )
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
        // Borrow the pre-scanned `(path, mtime)` pairs as `&Path` — no
        // PathBuf clone of the whole file list per cache-miss expansion.
        let (mut metadata, file_asts) = crate::collector::collect_metadata_from_files(scanned.iter().map(|(path, _)| path.as_path()), &folder_path, &processed.folder_name, route_storage).map_err(|e| syn::Error::new(Span::call_site(), format!("vespera! macro: failed to scan route folder '{}'. Error: {}. Check that all .rs files have valid Rust syntax.", processed.folder_name, e)))?;
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
        crate::parser::validate_schema_backed_extractors_with_cache(&metadata, &file_asts)?;
        stage("validate_schema_backed_extractors");

        let openapi = generate_and_write_openapi(
            processed,
            &metadata,
            file_asts,
            route_storage,
            &mut merge_specs,
        )?;
        stage("generate_and_write_openapi");

        let spec_json_hash = openapi.spec_json.as_deref().map(hash_str);
        let spec_pretty_hash = openapi.spec_pretty.as_deref().map(hash_str);
        let spec_pretty_fingerprint = write_pretty_sidecar(openapi.spec_pretty.as_deref());
        let embed_spec = write_spec_for_embedding(openapi.spec_json)?;
        let (spec_tokens, spec_json_fingerprint) =
            embed_spec.map_or((None, None), |spec| (Some(spec.tokens), spec.fingerprint));
        stage("write_spec_for_embedding");

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
                spec_json_hash,
                spec_pretty_hash,
                spec_json_fingerprint: spec_json_hash.and(spec_json_fingerprint),
                spec_pretty_fingerprint: spec_pretty_hash.and(spec_pretty_fingerprint),
            },
        );
        stage("write_cache");

        (metadata, spec_tokens)
    };

    // --- Cron job discovery from CRON_STORAGE ---
    // #[cron("...")] attribute already registers metadata at expansion time.
    // No folder scanning needed — just read the storage.
    let cron_jobs: Vec<crate::metadata::CronMetadata> = {
        // Per-crate snapshot (see `cron_impl::current_crate_crons`): in a
        // shared rust-analyzer proc-macro server this never picks up another
        // crate's `#[cron]` jobs.
        let storage = crate::cron_impl::current_crate_crons();
        let src_dir = std::env::var("CARGO_MANIFEST_DIR")
            .map(|d| {
                let p = std::path::PathBuf::from(d).join("src");
                // Canonicalize for reliable prefix stripping
                let canonical = p.canonicalize().unwrap_or(p);
                crate::file_utils::normalize_display_path(canonical)
            })
            .unwrap_or_default();
        let mut canonical_paths = HashMap::new();
        storage
            .iter()
            .map(|s| {
                // Derive module path from file_path relative to src/
                let module_path = s
                    .file_path
                    .as_ref()
                    .map(|fp| {
                        let normalized = canonicalized_cron_path(fp, &mut canonical_paths);
                        let relative = normalized
                            .strip_prefix(&src_dir)
                            .map_or(&*normalized, |rest| rest.trim_start_matches('/'));
                        // Convert path to module path: strip .rs, replace / with ::, strip mod
                        // Replace hyphens with underscores (Rust module convention)
                        cron_module_path(relative)
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

fn canonicalized_cron_path(fp: &str, cache: &mut HashMap<PathBuf, String>) -> String {
    let path = PathBuf::from(fp);
    cache
        .entry(path)
        .or_insert_with_key(|path| {
            path.canonicalize()
                .map_or_else(|_| fp.to_string(), |path| path.display().to_string())
                .replace('\\', "/")
        })
        .clone()
}

fn cron_module_path(relative: &str) -> String {
    let stem = relative.strip_suffix(".rs").unwrap_or(relative);
    let mut module_path = String::with_capacity(stem.len());
    for ch in stem.chars() {
        match ch {
            '/' => module_path.push_str("::"),
            '-' => module_path.push('_'),
            _ => module_path.push(ch),
        }
    }
    if module_path.ends_with("::mod") {
        module_path.truncate(module_path.len() - "::mod".len());
    }
    module_path
}

/// Process `export_app` macro - extracted for testability
#[allow(clippy::too_many_lines)]
pub fn process_export_app(
    name: &syn::Ident,
    folder_name: &str,
    schema_storage: &HashMap<String, StructMetadata>,
    manifest_dir: &str,
    route_storage: &[StoredRouteInfo],
    folder_span: Span,
) -> syn::Result<proc_macro2::TokenStream> {
    let profile_start = if std::env::var("VESPERA_PROFILE").is_ok() {
        Some(std::time::Instant::now())
    } else {
        None
    };

    let folder_path = find_folder_path(folder_name)?;
    if !folder_path.exists() {
        return Err(syn::Error::new(
            folder_span,
            format!(
                "export_app! macro: route folder '{folder_name}' not found. Create src/{folder_name} or specify a different folder with `dir = \"your_folder\"`.",
            ),
        ));
    }

    let app_name = name.to_string();
    let manifest_path = Path::new(manifest_dir);
    let target_dir = find_target_dir(manifest_path);
    let vespera_dir = target_dir.join("vespera");
    let spec_file = vespera_dir.join(format!("{app_name}.openapi.json"));
    let cache_path = get_export_cache_path(&app_name, folder_name);
    let scanned = crate::collector::scan_route_folder(&folder_path)
        .map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: {e}")))?;
    let fingerprints = crate::collector::fingerprints_from_scan(&scanned);
    let schema_hash = compute_schema_hash(schema_storage);
    let config_hash = compute_export_config_hash(&app_name, folder_name);
    let macro_version = env!("CARGO_PKG_VERSION").to_string();
    let macro_dev_fingerprint = compute_macro_dev_fingerprint();
    let cached = read_cache(&cache_path);
    let cache_hit = cached.as_ref().is_some_and(|c| {
        c.cache_format == CACHE_FORMAT
            && c.macro_version == macro_version
            && c.macro_dev_fingerprint == macro_dev_fingerprint
            && c.file_fingerprints == fingerprints
            && c.schema_hash == schema_hash
            && c.config_hash == config_hash
            && sidecar_matches(&spec_file, c.spec_json_hash, c.spec_json_fingerprint)
    });

    // Build a single fully-extended `metadata` per branch — previously the
    // cache-miss path did extend+merge+check twice (once inside this `else`
    // for the OpenAPI generation, once again in the outer block below) and
    // returned the un-extended snapshot, forcing the redundant re-work. Now
    // each branch returns the already-extended `CollectedMetadata` and the
    // duplicated outer pass is removed.
    let metadata = if let (true, Some(cache)) = (cache_hit, cached) {
        let mut metadata = cache.metadata;
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata.check_duplicate_schema_names().map_err(|msg| {
            syn::Error::new(Span::call_site(), format!("export_app! macro: {msg}"))
        })?;
        metadata
    } else {
        let (mut metadata, file_asts) = crate::collector::collect_metadata_from_files(scanned.iter().map(|(path, _)| path.as_path()), &folder_path, folder_name, route_storage).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to scan route folder '{folder_name}'. Error: {e}. Check that all .rs files have valid Rust syntax.")))?;
        let cache_metadata = metadata.clone();
        metadata.structs.extend(schema_storage.values().cloned());
        merge_route_storage_data(&mut metadata, route_storage);
        metadata.check_duplicate_schema_names().map_err(|msg| {
            syn::Error::new(Span::call_site(), format!("export_app! macro: {msg}"))
        })?;

        // B2: same-file extractor structs without `#[derive(Schema)]` would be
        // silently dropped from the spec — reject them at compile time.
        crate::parser::validate_schema_backed_extractors_with_cache(&metadata, &file_asts)?;

        // Generate OpenAPI spec JSON string
        let openapi_doc = crate::openapi_generator::try_generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            Some(file_asts),
            route_storage,
        )?;
        let spec_json = serde_json::to_string(&openapi_doc).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to serialize OpenAPI spec to JSON. Error: {e}. Check that all schema types are serializable.")))?;

        // Write spec to temp file for compile-time merging by parent apps
        std::fs::create_dir_all(&vespera_dir).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to create build cache directory '{}'. Error: {}. Ensure the target directory is writable.", vespera_dir.display(), e)))?;
        let spec_json_fingerprint = super::openapi_io::write_if_changed(&spec_file, &spec_json).map_err(|e| syn::Error::new(Span::call_site(), format!("export_app! macro: failed to write OpenAPI spec file '{}'. Error: {}. Ensure the file path is writable.", spec_file.display(), e)))?;
        let spec_json_hash = Some(hash_str(&spec_json));
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
                spec_json_hash,
                spec_pretty_hash: None,
                spec_json_fingerprint: spec_json_hash.and(spec_json_fingerprint),
                spec_pretty_fingerprint: None,
            },
        );
        metadata
    };
    let spec_path_str = crate::file_utils::path_to_include_str_literal(&spec_file);

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
mod tests;
