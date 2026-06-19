//! Collector for routes and structs

use std::collections::HashMap;
use std::path::Path;

use syn::Item;

mod path_scan;

pub use path_scan::normalize_path_key;
pub use path_scan::{fingerprints_from_scan, scan_route_folder};

use crate::{
    error::{MacroResult, err_call_site},
    file_utils::{collect_files, file_to_segments, normalize_display_path},
    metadata::{CollectedMetadata, RouteMetadata},
    route::{extract_doc_comment, extract_route_info},
    route_impl::StoredRouteInfo,
};

/// Kebab-case a route path for the file-based routing convention
/// (snake_case file / folder segments → kebab-case URL), but PRESERVE the
/// contents of `{...}` path parameters verbatim.  Hyphenating a `{user_id}`
/// parameter to `{user-id}` would corrupt the OpenAPI parameter name and
/// break the match with the handler's `Path` extractor, so underscores
/// inside `{...}` are left untouched.
fn kebab_case_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    for ch in path.chars() {
        match ch {
            '{' => {
                in_param = true;
                out.push(ch);
            }
            '}' => {
                in_param = false;
                out.push(ch);
            }
            '_' if !in_param => out.push('-'),
            other => out.push(other),
        }
    }
    out
}

/// Collect routes and structs from a folder.
///
/// When `route_storage` contains entries with `file_path`, files covered by
/// `ROUTE_STORAGE` skip expensive `syn::parse_file()` — route metadata is built
/// directly from the stored data. Default values for `serde(default = "fn")`
/// are already extracted by `#[derive(Schema)]` into `SCHEMA_STORAGE.field_defaults`.
///
/// Returns the metadata AND the parsed file ASTs, so downstream consumers
/// (e.g., `openapi_generator`) can reuse them without re-reading files from disk.
#[allow(clippy::option_if_let_else, clippy::too_many_lines)]
pub fn collect_metadata(
    folder_path: &Path,
    folder_name: &str,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<(CollectedMetadata, HashMap<String, syn::File>)> {
    let files = collect_files(folder_path).map_err(|e| err_call_site(format!("vespera! macro: failed to scan route folder '{}': {}. Verify the folder exists and is readable.", folder_path.display(), e)))?;
    collect_metadata_from_files(
        files.iter().map(std::path::PathBuf::as_path),
        folder_path,
        folder_name,
        route_storage,
    )
}

/// [`collect_metadata`] over a **pre-scanned** file list — lets
/// `vespera!` reuse the single directory walk it already performed
/// for cache fingerprinting instead of walking the folder twice.
#[allow(clippy::option_if_let_else, clippy::too_many_lines)]
pub fn collect_metadata_from_files<'a>(
    files: impl IntoIterator<Item = &'a Path>,
    folder_path: &Path,
    folder_name: &str,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<(CollectedMetadata, HashMap<String, syn::File>)> {
    let mut metadata = CollectedMetadata::new();

    // Borrows the caller's path source (slice or pre-scanned `(path, mtime)`
    // pairs) by `&Path`, so neither `vespera!` (cache miss) nor
    // `collect_metadata` needs to clone the path list. `file_asts` only holds
    // slow-path (non-ROUTE_STORAGE) parses, so a default-capacity map is fine.
    let mut file_asts = HashMap::new();

    // Index ROUTE_STORAGE entries by **canonicalized** file path for O(1)
    // lookup.  `#[route]` records `Span::local_file()`, which rustc
    // reports relative to its invocation directory (e.g.
    // `src\routes\users.rs`), while the collector walks
    // `{CARGO_MANIFEST_DIR}/src/{folder}` producing absolute paths with
    // platform separators.  Comparing the raw strings never matches —
    // silently disabling the fast path and re-parsing every route file
    // on each cache miss.  Canonicalizing both sides makes the keys
    // comparable regardless of cwd-relativity or separator style.
    let cwd = std::env::current_dir().unwrap_or_default();
    let storage_by_file: HashMap<String, Vec<&StoredRouteInfo>> = {
        let mut map: HashMap<String, Vec<&StoredRouteInfo>> = HashMap::new();
        for stored in route_storage {
            if let Some(ref fp) = stored.file_path {
                map.entry(normalize_path_key(fp, &cwd))
                    .or_default()
                    .push(stored);
            }
        }
        map
    };

    for file in files {
        if file.extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let mut file_path = normalize_display_path(file);

        let segments = file
            .strip_prefix(folder_path)
            .map(|file_stem| file_to_segments(file_stem, folder_path))
            .map_err(|e| {
                err_call_site(format!(
                    "Failed to strip prefix from file: {} (base: {}): {}",
                    file.display(),
                    folder_path.display(),
                    e
                ))
            })?;

        let mut module_path = if folder_name.is_empty() {
            segments.join("::")
        } else {
            format!("{}::{}", folder_name, segments.join("::"))
        };

        let base_path = format!("/{}", segments.join("/"));

        // Fast path: ROUTE_STORAGE has entries for this file — skip syn::parse_file()
        //
        // Per-file invariants (`module_path`, `file_path`) are CLONED for
        // every non-last route but MOVED into the last route's push —
        // refcount-free amortization of two String allocations per file.
        if let Some(stored_routes) = storage_by_file.get(&normalize_path_key(&file_path, &cwd)) {
            let n = stored_routes.len();
            for (i, stored) in stored_routes.iter().enumerate() {
                let route_path = if let Some(ref custom_path) = stored.custom_path {
                    let trimmed_base = base_path.trim_end_matches('/');
                    format!("{trimmed_base}/{}", custom_path.trim_start_matches('/'))
                } else {
                    base_path.clone()
                };
                let route_path = kebab_case_path(&route_path);

                // `#[route]` already resolved the description at expansion
                // time (explicit attribute OR doc comment — see
                // `process_route_attribute`), so `stored.description` is
                // authoritative.  Re-parsing `fn_sig_str` here could never
                // find a doc comment the attribute macro didn't.
                let description = stored.description.clone();

                let (mp, fp) = if i + 1 == n {
                    (
                        std::mem::take(&mut module_path),
                        std::mem::take(&mut file_path),
                    )
                } else {
                    (module_path.clone(), file_path.clone())
                };

                metadata.routes.push(RouteMetadata {
                    // `#[route]` bare form defaults to GET — mirror the
                    // slow path (`route::utils`), which resolves a
                    // missing method to "get".  `unwrap_or_default()`
                    // produced "" here, silently dropping such routes
                    // from the OpenAPI doc when the fast path is active.
                    method: stored.method.clone().unwrap_or_else(|| "get".to_string()),
                    path: route_path,
                    function_name: stored.fn_name.clone(),
                    module_path: mp,
                    file_path: fp,
                    success_status: stored.success_status,
                    error_status: stored.error_status.clone(),
                    typed_responses: stored.typed_responses.clone(),
                    tags: stored.tags.clone(),
                    security: stored.security.clone(),
                    headers: stored.headers.clone(),
                    operation_id: stored.operation_id.clone(),
                    summary: stored.summary.clone(),
                    request_example: stored.request_example.clone(),
                    response_example: stored.response_example.clone(),
                    deprecated: stored.deprecated,
                    description,
                });
            }

            // No file_asts insertion needed in fast path:
            // #[derive(Schema)] already extracts serde(default = "fn") values
            // into SCHEMA_STORAGE.field_defaults (Priority 0 in process_default_functions)
        } else {
            let file_ast = crate::schema_macro::file_cache::get_parsed_file(file).ok_or_else(|| err_call_site(format!("vespera! macro: cannot read or parse '{}'. Fix the Rust syntax errors in this file.", file.display())))?;

            file_asts.insert(file_path.clone(), file_ast);
            let file_ast = &file_asts[&file_path];

            // Pre-collect (fn_item, owned RouteInfo) pairs so we can
            //   1. detect the last route up-front (symmetric with fast path),
            //   2. MOVE owned RouteInfo fields (method / error_status / tags /
            //      description) into RouteMetadata instead of re-cloning them.
            let mut route_entries: Vec<(&syn::ItemFn, crate::route::RouteInfo)> = Vec::new();
            for item in &file_ast.items {
                if let Item::Fn(fn_item) = item
                    && let Some(route_info) = extract_route_info(&fn_item.attrs)
                {
                    route_entries.push((fn_item, route_info));
                }
            }

            let n = route_entries.len();
            for (i, (fn_item, route_info)) in route_entries.into_iter().enumerate() {
                let route_path = if let Some(custom_path) = &route_info.path {
                    let trimmed_base = base_path.trim_end_matches('/');
                    format!("{trimmed_base}/{}", custom_path.trim_start_matches('/'))
                } else {
                    base_path.clone()
                };
                let route_path = kebab_case_path(&route_path);

                // Description priority: route attribute > doc comment
                // (move the owned Option instead of cloning + dropping it)
                let description = route_info
                    .description
                    .or_else(|| extract_doc_comment(&fn_item.attrs));

                let (mp, fp) = if i + 1 == n {
                    (
                        std::mem::take(&mut module_path),
                        std::mem::take(&mut file_path),
                    )
                } else {
                    (module_path.clone(), file_path.clone())
                };

                metadata.routes.push(RouteMetadata {
                    method: route_info.method,
                    path: route_path,
                    function_name: fn_item.sig.ident.to_string(),
                    module_path: mp,
                    file_path: fp,
                    success_status: route_info.success_status,
                    error_status: route_info.error_status,
                    typed_responses: route_info.typed_responses,
                    tags: route_info.tags,
                    security: route_info.security,
                    headers: route_info.headers,
                    operation_id: route_info.operation_id,
                    summary: route_info.summary,
                    request_example: route_info.request_example,
                    response_example: route_info.response_example,
                    deprecated: route_info.deprecated,
                    description,
                });
            }
        }
    }

    Ok((metadata, file_asts))
}

#[cfg(test)]
mod tests;
