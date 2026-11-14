//! Collector for routes and structs

use std::collections::HashMap;
use std::path::Path;

use syn::Item;

mod module_scope;
mod path_scan;

pub use path_scan::{fingerprints_from_scan, scan_route_folder};

use crate::{
    error::{MacroResult, err_call_site},
    file_utils::{file_to_segments, normalize_display_path, normalize_path_key},
    metadata::{CfgRoute, CollectedMetadata, RouteMetadata},
    route::{doc_summary_and_description, extract_route_info, has_deprecated_attr},
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

/// Join a file-derived `base_path` with an optional `#[route(path = "...")]`
/// suffix.
///
/// Only the file/module-derived `base_path` is kebab-cased (the file-based
/// routing convention: `user_groups.rs` → `/user-groups`). An explicit
/// `path = "..."` is the author's literal URL and is joined verbatim —
/// `#[route(path = "/my_route")]` serves `/my_route`.
///
/// Both collection paths (the `ROUTE_STORAGE` fast path and the `syn` slow
/// path) MUST produce byte-identical route paths for the same route — a
/// one-sided change would make the OpenAPI document depend on whether the
/// fast path happened to be active.  Keeping the join in exactly one place
/// makes that divergence unrepresentable.
fn build_route_path(base_path: &str, custom_path: Option<&str>) -> String {
    let base = kebab_case_path(base_path);
    match custom_path.map(|custom| custom.trim_start_matches('/')) {
        // `path = "/"` (or `""`) is the file route itself: `users.rs` serves
        // `/users`, not a `/users/` trailing-slash twin.
        None | Some("") => base,
        Some(custom) => format!("{}/{custom}", base.trim_end_matches('/')),
    }
}

/// Yield the per-file invariants (`module_path`, `file_path`) for one route.
///
/// The last route of a file MOVES them out (leaving empty `String`s behind);
/// every earlier route CLONES — refcount-free amortization of two `String`
/// allocations per file.
///
/// Both collection paths (the `ROUTE_STORAGE` fast path and the `syn` slow
/// path) need exactly this behaviour, and it must stay identical between them
/// for the same reason [`build_route_path`] documents: a one-sided change
/// would make the emitted metadata depend on which path happened to run.
#[inline]
fn take_or_clone(
    module_path: &mut String,
    file_path: &mut String,
    is_last: bool,
) -> (String, String) {
    if is_last {
        (std::mem::take(module_path), std::mem::take(file_path))
    } else {
        (module_path.clone(), file_path.clone())
    }
}

/// Fast path: push one file's routes straight from its `ROUTE_STORAGE`
/// entries, skipping `syn::parse_file()` entirely.
///
/// `module_path` / `file_path` are the per-file invariants; the last route
/// MOVES them out via [`take_or_clone`], every earlier route clones.
fn push_stored_routes(
    metadata: &mut CollectedMetadata,
    base_path: &str,
    module_path: &mut String,
    file_path: &mut String,
    stored_routes: &[&StoredRouteInfo],
) {
    let n = stored_routes.len();
    for (i, stored) in stored_routes.iter().enumerate() {
        let route_path = build_route_path(base_path, stored.custom_path.as_deref());

        // `#[route]` already resolved the description at expansion
        // time (explicit attribute OR doc comment — see
        // `process_route_attribute`), so `stored.description` is
        // authoritative.  Re-parsing `fn_sig_str` here could never
        // find a doc comment the attribute macro didn't.
        let description = stored.description.clone();

        let (mp, fp) = take_or_clone(module_path, file_path, i + 1 == n);

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
}

/// Slow path: push one file's routes by walking its already-parsed `syn` AST.
///
/// Field-for-field and order-for-order this MUST stay identical to
/// [`push_stored_routes`] — see the invariant documented on
/// [`build_route_path`] and [`take_or_clone`].
///
/// Handlers gated by `module_cfg` (the file's module declaration chain) or
/// by their own `#[cfg(..)]` go to `metadata.cfg_routes` instead — see
/// [`CfgRoute`].
fn push_parsed_routes(
    metadata: &mut CollectedMetadata,
    base_path: &str,
    module_path: &mut String,
    file_path: &mut String,
    file_ast: &syn::File,
    module_cfg: &[String],
) {
    // Pre-collect (fn_item, owned RouteInfo) pairs so we can
    //   1. detect the last route up-front (symmetric with fast path),
    //   2. MOVE owned RouteInfo fields (method / error_status / tags /
    //      description) into RouteMetadata instead of re-cloning them.
    let mut route_entries: Vec<(&syn::ItemFn, crate::route::RouteInfo)> = Vec::new();
    for item in &file_ast.items {
        let Item::Fn(fn_item) = item else { continue };
        let Some(route_info) = extract_route_info(&fn_item.attrs) else {
            continue;
        };
        let mut cfg = module_cfg.to_vec();
        cfg.extend(module_scope::cfg_predicates(&fn_item.attrs));
        if cfg.is_empty() {
            route_entries.push((fn_item, route_info));
        } else {
            metadata.cfg_routes.push(CfgRoute {
                cfg,
                path: build_route_path(base_path, route_info.path.as_deref()),
                method: route_info.method,
                module_path: module_path.clone(),
                function_name: fn_item.sig.ident.to_string(),
                file_path: file_path.clone(),
            });
        }
    }

    let n = route_entries.len();
    for (i, (fn_item, route_info)) in route_entries.into_iter().enumerate() {
        let route_path = build_route_path(base_path, route_info.path.as_deref());

        // Summary / description: route attributes > doc comment (first
        // line → summary, rest → description) — same rule as `#[route]`.
        let (summary, description) =
            doc_summary_and_description(route_info.summary, route_info.description, &fn_item.attrs);

        let (mp, fp) = take_or_clone(module_path, file_path, i + 1 == n);

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
            summary,
            request_example: route_info.request_example,
            response_example: route_info.response_example,
            deprecated: route_info.deprecated || has_deprecated_attr(&fn_item.attrs),
            description,
        });
    }
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
// Test-only convenience wrapper: `vespera!` / `export_app!` reach the collector
// through `collect_metadata_from_files` (which reuses the cache's single
// directory walk), so this folder-walking variant exists purely for the unit
// tests that exercise the collector end-to-end. `#[cfg(test)]` keeps it (and its
// `collect_files` dependency) out of the shipped proc-macro entirely.
#[cfg(test)]
pub fn collect_metadata(
    folder_path: &Path,
    folder_name: &str,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<(CollectedMetadata, HashMap<String, syn::File>)> {
    let files = crate::file_utils::collect_files(folder_path).map_err(|e| err_call_site(format!("vespera! macro: failed to scan route folder '{}': {}. Verify the folder exists and is readable.", folder_path.display(), e)))?;
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
    let mut storage_by_file: HashMap<String, Vec<&StoredRouteInfo>> = HashMap::new();
    for stored in route_storage {
        if let Some(ref fp) = stored.file_path {
            storage_by_file
                .entry(normalize_path_key(fp, &cwd))
                .or_default()
                .push(stored);
        }
    }

    let folder_module = crate::router_codegen::route_dir_module_path(folder_name);
    for file in files {
        if file.extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let mut file_path = normalize_display_path(file);
        // Fast-path lookup key, computed once and reused below.  Feeding the
        // already-built `file_path` borrow (not a fresh `file.to_string_lossy()`)
        // avoids an extra owned-string allocation; `normalize_path_key` does its
        // own separator + component folding, so the key is identical either way.
        let file_key = normalize_path_key(&file_path, &cwd);

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
            format!("{}::{}", folder_module, segments.join("::"))
        };

        let base_path = format!("/{}", segments.join("/"));

        // Fast path: ROUTE_STORAGE has entries for this file — skip syn::parse_file()
        //
        // Per-file invariants (`module_path`, `file_path`) are CLONED for
        // every non-last route but MOVED into the last route's push —
        // refcount-free amortization of two String allocations per file.
        if let Some(stored_routes) = storage_by_file.get(&file_key) {
            if let Some(found) = module_scope::stored_routes_inline_module(file, stored_routes) {
                return Err(err_call_site(module_scope::inline_mod_route_error(
                    file, &found,
                )));
            }
            push_stored_routes(
                &mut metadata,
                &base_path,
                &mut module_path,
                &mut file_path,
                stored_routes,
            );

            // No file_asts insertion needed in fast path:
            // #[derive(Schema)] already extracts serde(default = "fn") values
            // into SCHEMA_STORAGE.field_defaults (Priority 0 in process_default_functions)
        } else {
            let parsed = crate::schema_macro::file_cache::get_parsed_file(file);
            let module_cfg = module_scope::module_cfg_predicates(folder_path, &segments);
            if !module_cfg.is_empty() {
                // Never registered by `#[route]` and declared behind
                // `#[cfg(..)]`: compiled out, or expanded after `vespera!`.
                // rustc never reads a compiled-out file, so an unparsable one
                // contributes nothing.
                if let Some(file_ast) = parsed {
                    push_parsed_routes(
                        &mut metadata,
                        &base_path,
                        &mut module_path,
                        &mut file_path,
                        &file_ast,
                        &module_cfg,
                    );
                }
                continue;
            }
            let file_ast = parsed.ok_or_else(|| err_call_site(format!("vespera! macro: cannot read or parse '{}'. Fix the Rust syntax errors in this file.", file.display())))?;

            // `entry` hashes and probes `file_path` ONCE and hands back a
            // borrow of the slot it just filled; the previous
            // `insert(..) + &file_asts[&file_path]` pair re-hashed and
            // re-probed the same key purely to re-borrow the value it had
            // just moved in.  It also removes an `Index` panic site, which
            // vespera_macro/AGENTS.md forbids in the collector.
            //
            // The `clone()` stays: the map needs to own its key, because
            // `push_parsed_routes` takes `&mut file_path` and may `mem::take`
            // it for the file's last route.
            if let Some(found) = module_scope::inline_mod_route(&file_ast.items, &|_| false) {
                return Err(err_call_site(module_scope::inline_mod_route_error(
                    file, &found,
                )));
            }
            let file_ast: &syn::File = file_asts.entry(file_path.clone()).or_insert(file_ast);

            push_parsed_routes(
                &mut metadata,
                &base_path,
                &mut module_path,
                &mut file_path,
                file_ast,
                &[],
            );
        }
    }

    Ok((metadata, file_asts))
}

#[cfg(test)]
mod tests;
