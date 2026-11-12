//! Build `PathItem`s from collected route metadata.
//!
//! This module owns the parallel fan-out infrastructure used during
//! OpenAPI generation:
//!
//! * [`PARALLEL_THRESHOLD`] / [`parallel_filter_map`] — `filter_map`
//!   across worker threads, with a sequential fast-path below
//!   `PARALLEL_THRESHOLD`.
//! * [`FallbackGuard`] — forces proc-macro2's thread-safe fallback
//!   implementation while workers parse `syn` source strings.
//! * [`run_route_jobs_parallel`] — convenience wrapper around
//!   `parallel_filter_map` for [`RouteJob`] → [`BuiltOperation`].
//!
//! Both `build_path_items` (route signatures) and
//! `parse_component_schemas` (struct definitions) drive worker pools
//! through `parallel_filter_map`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vespera_core::route::{HttpMethod, PathItem};

use crate::{
    file_utils::normalize_path_key,
    metadata::CollectedMetadata,
    parser::{
        OperationRouteConfig, build_operation_from_function, openapi_path_template,
        schema::generic_instances,
    },
    route_impl::StoredRouteInfo,
};

type FnIndex<'a> = HashMap<String, HashMap<String, &'a syn::ItemFn>>;
type StorageFnSigs<'a> = HashMap<(Option<String>, &'a str), Option<&'a str>>;

/// Build path items and collect tags from route metadata.
///
/// Uses `route_storage` (from `#[route]` macro) as the primary source for function
/// signatures. Falls back to pre-built `file_cache` when ROUTE_STORAGE doesn't
/// have an entry (e.g., during tests or for routes added without the attribute).
/// An AST-backed route job: route index, metadata, signature, and the AST of
/// the file that declares it (for same-file alias lookup).
type AstJob<'a> = (
    usize,
    &'a crate::metadata::RouteMetadata,
    &'a syn::Signature,
    Option<&'a syn::File>,
);

/// `sig` with its return-type alias expanded (borrowed when nothing changed).
fn expanded_signature<'a>(
    sig: &'a syn::Signature,
    file_ast: Option<&syn::File>,
    src_dir: Option<&std::path::Path>,
) -> std::borrow::Cow<'a, syn::Signature> {
    let mut expanded = sig.clone();
    if crate::parser::expand_return_type_alias(&mut expanded, file_ast, src_dir) {
        std::borrow::Cow::Owned(expanded)
    } else {
        std::borrow::Cow::Borrowed(sig)
    }
}

pub(super) fn build_path_items(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<(BTreeMap<String, PathItem>, BTreeSet<String>)> {
    let mut paths = BTreeMap::new();
    let mut all_tags = BTreeSet::new();

    // Compute once: `cwd` anchors every path normalization below so the
    // three path sources — `file_cache` keys (collector), route metadata
    // spans, and ROUTE_STORAGE `#[route]` spans — compare in one canonical
    // space (separator/relativity/case can differ, especially on Windows).
    let cwd = std::env::current_dir().unwrap_or_default();

    // Build the file-AST function index FIRST so the storage path
    // below can skip any function whose AST is already reachable through
    // `file_cache`.  `collector::collect_metadata` has already walked
    // these files via `syn::parse_file`, so re-parsing `fn_sig_str`
    // from ROUTE_STORAGE for the same function is pure duplicated work.
    //
    // Keyed by the NORMALIZED path so the `already_in_ast` storage check
    // and the main-loop AST lookup match regardless of path format — a raw
    // key misses when the `#[route]` span path differs from the collector's
    // `file_cache` key, needlessly re-parsing the signature on a worker.
    let fn_index: FnIndex<'_> = file_cache
        .iter()
        .map(|(path, ast)| {
            let fns: HashMap<String, &syn::ItemFn> = ast
                .items
                .iter()
                .filter_map(|item| {
                    if let syn::Item::Fn(fn_item) = item {
                        Some((fn_item.sig.ident.to_string(), fn_item))
                    } else {
                        None
                    }
                })
                .collect();
            (normalize_path_key(path, &cwd), fns)
        })
        .collect();
    let file_by_key: HashMap<String, &syn::File> = file_cache
        .iter()
        .map(|(path, ast)| (normalize_path_key(path, &cwd), ast))
        .collect();

    // ROUTE_STORAGE-backed function signatures (skipped when the same
    // function is already covered by `fn_index` — re-parsing would be
    // duplicated work).  These are plain *strings*, so the expensive
    // `syn::parse_str` + operation build runs on worker threads below;
    // `syn` ASTs are not `Send`, which is also why fn_index-backed
    // routes stay on this thread.
    let storage_fn_sigs = build_storage_fn_sigs(route_storage, &fn_index, &cwd);

    // Split routes by signature source. `idx` preserves the original
    // route order so PathItem operations are applied deterministically
    // regardless of which thread produced them.
    let mut parallel_jobs: Vec<(usize, &crate::metadata::RouteMetadata, &str)> = Vec::new();
    let mut ast_jobs: Vec<AstJob<'_>> = Vec::new();
    for (idx, route_meta) in metadata.routes.iter().enumerate() {
        // ROUTE_STORAGE first (avoids file_cache dependency for known
        // routes) — same priority order as the previous sequential code.
        //
        // `normalize_path_key` canonicalises the path (allocates + folds
        // `.`/`..` components + display-renders + Windows case-folds), so
        // compute it ONCE per route and reuse the owned key: the storage
        // lookup takes it by reference, and on a storage miss the `fn_index`
        // fallback MOVES the same `String` out of `storage_key.0` instead of
        // recomputing it.  The prior code ran the full normalization twice
        // per route.
        let storage_key = (
            Some(normalize_path_key(&route_meta.file_path, &cwd)),
            route_meta.function_name.as_str(),
        );
        let legacy_storage_key = (None, route_meta.function_name.as_str());
        if let Some(fn_sig_str) = storage_fn_sigs
            .get(&storage_key)
            .copied()
            .flatten()
            .or_else(|| storage_fn_sigs.get(&legacy_storage_key).copied().flatten())
        {
            parallel_jobs.push((idx, route_meta, fn_sig_str));
        } else if let Some(norm_key) = storage_key.0
            && let Some(fns) = fn_index.get(&norm_key)
            && let Some(fn_item) = fns.get(&route_meta.function_name)
        {
            ast_jobs.push((
                idx,
                route_meta,
                &fn_item.sig,
                file_by_key.get(&norm_key).copied(),
            ));
        }
    }

    let build_one = |route_meta: &crate::metadata::RouteMetadata,
                     fn_sig: &syn::Signature|
     -> syn::Result<Option<(HttpMethod, vespera_core::route::Operation)>> {
        let Ok(method) = HttpMethod::try_from(route_meta.method.as_str()) else {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "vespera: route '{}' has unsupported HTTP method '{}'. Supported methods are GET, POST, PUT, PATCH, DELETE, HEAD, and OPTIONS.",
                    route_meta.path, route_meta.method
                ),
            ));
        };
        let mut operation = build_operation_from_function(
            fn_sig,
            &route_meta.path,
            known_schema_names,
            struct_definitions,
            OperationRouteConfig {
                error_status: route_meta.error_status.as_deref(),
                typed_responses: route_meta.typed_responses.as_deref(),
                success_status: route_meta.success_status,
                tags: route_meta.tags.as_deref(),
                security: route_meta.security.as_deref(),
                headers: Some(&route_meta.headers),
                operation_id: route_meta.operation_id.as_deref(),
                summary: route_meta.summary.as_deref(),
                request_example: route_meta.request_example.as_ref(),
                response_example: route_meta.response_example.as_ref(),
                deprecated: route_meta.deprecated,
            },
        );
        operation.description.clone_from(&route_meta.description);
        Ok(Some((method, operation)))
    };

    // Parse + build string-backed routes on worker threads.  Workers
    // produce only `Send` data (`Operation` is plain `vespera_core`
    // data); `syn` parsing inside a worker uses proc-macro2's fallback
    // implementation, which is thread-safe.
    let mut results: Vec<(usize, HttpMethod, vespera_core::route::Operation)> =
        run_route_jobs_parallel(&parallel_jobs, &build_one)?;

    // `#[route]` already expanded return-type aliases in the stored
    // signatures; AST-backed signatures get the same expansion here.
    let src_dir = crate::schema_macro::file_cache::get_manifest_dir()
        .map(|dir| std::path::PathBuf::from(dir).join("src"));
    for (idx, route_meta, fn_sig, file_ast) in ast_jobs {
        let fn_sig = expanded_signature(fn_sig, file_ast, src_dir.as_deref());
        if let Some((method, operation)) = build_one(route_meta, &fn_sig)? {
            results.push((idx, method, operation));
        }
    }

    // Deterministic assembly in original route order.
    results.sort_unstable_by_key(|(idx, _, _)| *idx);
    assemble_path_items(results, metadata, &mut paths, &mut all_tags)?;

    Ok((paths, all_tags))
}

/// Apply built operations to their `PathItem`s in route order, rejecting a
/// duplicate `(method, path)` with a compile error that names BOTH conflicting
/// handlers.  Previously `set_operation` silently discarded the earlier
/// operation — dropping a route from the generated spec with no diagnostic.
/// axum itself panics on a duplicate method+path at runtime, so surfacing it at
/// compile time is strictly better than the silent loss.
fn assemble_path_items(
    mut results: Vec<(usize, HttpMethod, vespera_core::route::Operation)>,
    metadata: &CollectedMetadata,
    paths: &mut BTreeMap<String, PathItem>,
    all_tags: &mut BTreeSet<String>,
) -> syn::Result<()> {
    resolve_operation_ids(&mut results, metadata)?;
    // Borrows from `metadata`, which outlives this loop — the map is only ever
    // read inside the duplicate-route error branch below, so cloning the path
    // and handler name for every route was two wasted allocations per route.
    let mut claimed: HashMap<(&str, HttpMethod), &str> = HashMap::new();
    for (idx, method, operation) in results {
        let route_meta = &metadata.routes[idx];
        if let Some(tags) = &route_meta.tags {
            for tag in tags {
                all_tags.insert(tag.clone());
            }
        }
        let path_item = paths
            .entry(openapi_path_template(&route_meta.path))
            .or_default();
        if path_item.try_set_operation(method, operation).is_some() {
            let previous = claimed
                .get(&(route_meta.path.as_str(), method))
                .copied()
                .unwrap_or("<unknown>");
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "duplicate route: `{method} {path}` is defined by both `{previous}` and \
                     `{current}` — each (method, path) pair must map to exactly one handler",
                    path = route_meta.path,
                    current = route_meta.function_name,
                ),
            ));
        }
        claimed.insert(
            (route_meta.path.as_str(), method),
            route_meta.function_name.as_str(),
        );
    }
    Ok(())
}

/// Make every operationId unique (OpenAPI: "operationId MUST be unique").
///
/// The id defaults to the handler's fn ident, and file-based routing routinely
/// produces `routes/users.rs::list` + `routes/posts.rs::list`.  When DEFAULT
/// ids collide, every colliding default is qualified with its module path
/// (relative to the routes folder) joined by `_` — `users_list`, `posts_list`,
/// `admin_users_list`.  Non-colliding defaults stay untouched so existing specs
/// do not churn.  An explicit `operation_id = "..."` is never rewritten: a clash
/// involving one (or a clash that survives qualification) is a compile error
/// naming both handlers.
fn resolve_operation_ids(
    results: &mut [BuiltOperation],
    metadata: &CollectedMetadata,
) -> syn::Result<()> {
    let is_default = |idx: usize| metadata.routes[idx].operation_id.is_none();
    let mut default_counts: HashMap<&str, usize> = HashMap::new();
    for (idx, _, operation) in results.iter() {
        if is_default(*idx)
            && let Some(id) = operation.operation_id.as_deref()
        {
            *default_counts.entry(id).or_default() += 1;
        }
    }
    let colliding: HashSet<String> = default_counts
        .into_iter()
        .filter(|&(_, count)| count > 1)
        .map(|(id, _)| id.to_owned())
        .collect();
    if !colliding.is_empty() {
        let common = common_module_prefix_len(&metadata.routes);
        for (idx, _, operation) in results.iter_mut() {
            if is_default(*idx)
                && let Some(id) = operation.operation_id.as_mut()
                && colliding.contains(id.as_str())
            {
                *id = qualified_operation_id(&metadata.routes[*idx], id, common);
            }
        }
    }

    let mut claimed: HashMap<&str, usize> = HashMap::new();
    // `build_operation` always sets an id (explicit or the fn name); an
    // operation without one simply has nothing to claim.
    let with_ids = results.iter().filter_map(|(idx, method, operation)| {
        Some((idx, method, operation.operation_id.as_deref()?))
    });
    for (idx, method, operation_id) in with_ids {
        if let Some(&previous_idx) = claimed.get(operation_id) {
            let previous = &metadata.routes[previous_idx];
            let route_meta = &metadata.routes[*idx];
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "duplicate operationId `{operation_id}`: used by both `{prev_handler}` \
                     (`{prev_method} {prev_path}`) and `{handler}` (`{method} {path}`) — \
                     operationIds must be unique; set one explicitly with \
                     `#[vespera::route(..., operation_id = \"...\")]`",
                    prev_handler = handler_display(previous),
                    prev_method = previous.method.to_ascii_uppercase(),
                    prev_path = previous.path,
                    handler = handler_display(route_meta),
                    path = route_meta.path,
                ),
            ));
        }
        claimed.insert(operation_id, *idx);
    }
    Ok(())
}

/// Non-empty `::` segments of a route's module path.
fn module_segments(route: &crate::metadata::RouteMetadata) -> Vec<&str> {
    route
        .module_path
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Number of leading module segments shared by EVERY route — normally the
/// routes folder name the collector prepends (`routes::users` → `routes`).
fn common_module_prefix_len(routes: &[crate::metadata::RouteMetadata]) -> usize {
    let mut all = routes.iter().map(module_segments);
    // No routes → no shared prefix (callers only ask when ids collide, i.e.
    // with at least two routes, but `0` is the honest answer regardless).
    all.next().map_or(0, |first| {
        all.fold(first.len(), |common, segments| {
            first
                .iter()
                .zip(&segments)
                .take(common)
                .take_while(|(a, b)| a == b)
                .count()
        })
    })
}

/// `{module segments after the routes folder}_{default id}`.  The collector
/// prefixes every module path with the routes folder name (`routes::admin::users`),
/// so one leading segment is dropped when ALL routes share it.  At least one
/// module segment is always kept so a qualified id never degrades back to the
/// bare fn name; non-identifier characters become `_`.
fn qualified_operation_id(
    route: &crate::metadata::RouteMetadata,
    default_id: &str,
    common: usize,
) -> String {
    let segments = module_segments(route);
    let skip = common.min(1).min(segments.len().saturating_sub(1));
    let mut qualified = String::new();
    for segment in &segments[skip..] {
        qualified.extend(segment.chars().map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        }));
        qualified.push('_');
    }
    qualified.push_str(default_id);
    qualified
}

/// `module::path::handler` for diagnostics (just `handler` at the crate root).
fn handler_display(route: &crate::metadata::RouteMetadata) -> String {
    if route.module_path.is_empty() {
        route.function_name.clone()
    } else {
        format!("{}::{}", route.module_path, route.function_name)
    }
}

/// Find the first `operationId` shared by two operations across `paths`.
///
/// Used after merging child-app specs (`merge = [...]`), where only the
/// serialized documents — not handler names — are available. Returns
/// `(operation_id, first "METHOD /path", second "METHOD /path")`.
pub fn find_duplicate_operation_id(
    paths: &BTreeMap<String, PathItem>,
) -> Option<(String, String, String)> {
    let mut seen: HashMap<&str, String> = HashMap::new();
    for (path, item) in paths {
        for (method, operation) in item.operations() {
            let Some(operation_id) = operation.operation_id.as_deref() else {
                continue;
            };
            let location = format!("{method} {path}");
            if let Some(previous) = seen.get(operation_id) {
                return Some((operation_id.to_string(), previous.clone(), location));
            }
            seen.insert(operation_id, location);
        }
    }
    None
}

fn build_storage_fn_sigs<'a>(
    route_storage: &'a [StoredRouteInfo],
    fn_index: &FnIndex<'_>,
    cwd: &std::path::Path,
) -> StorageFnSigs<'a> {
    let mut storage = HashMap::with_capacity(route_storage.len());
    for s in route_storage {
        // Canonicalise the stored path ONCE per route (it allocates + folds
        // path components + display-renders) and reuse it for both the
        // `already_in_ast` skip check (by reference) and the storage key (by
        // move) — the prior code ran the full normalization twice per route.
        let norm_fp = s.file_path.as_deref().map(|fp| normalize_path_key(fp, cwd));
        let already_in_ast = norm_fp
            .as_ref()
            .and_then(|fp| fn_index.get(fp))
            .is_some_and(|fns| fns.contains_key(&s.fn_name));
        if already_in_ast {
            continue;
        }
        let key = (norm_fp, s.fn_name.as_str());
        storage
            .entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(s.fn_sig_str.as_str()));
    }
    storage
}

/// Run string-backed route-operation builds across worker threads.
///
/// Sequential below [`PARALLEL_THRESHOLD`] jobs — thread spawn overhead
/// dominates tiny projects.  Chunked `std::thread::scope` otherwise
/// (zero new dependencies).
pub(super) const PARALLEL_THRESHOLD: usize = 16;

/// `(original route index, route metadata, fn signature source)` job input.
pub(super) type RouteJob<'a> = (usize, &'a crate::metadata::RouteMetadata, &'a str);

/// `(original route index, resolved method, built operation)` result.
pub(super) type BuiltOperation = (usize, HttpMethod, vespera_core::route::Operation);

/// Builds one operation from a route's resolved fn signature.
pub(super) type OperationBuilder<'a> = dyn Fn(
        &crate::metadata::RouteMetadata,
        &syn::Signature,
    ) -> syn::Result<Option<(HttpMethod, vespera_core::route::Operation)>>
    + Sync
    + 'a;

/// RAII restore for [`proc_macro2::fallback::force`] — releases the
/// forced fallback mode even when a worker panics.
struct FallbackGuard;

impl Drop for FallbackGuard {
    fn drop(&mut self) {
        proc_macro2::fallback::unforce();
    }
}

fn run_route_jobs_parallel(
    jobs: &[RouteJob<'_>],
    build_one: &OperationBuilder<'_>,
) -> syn::Result<Vec<BuiltOperation>> {
    parallel_filter_map(jobs, &|&(idx, route_meta, fn_sig_str): &RouteJob<'_>| {
        let fn_sig = syn::parse_str::<syn::Signature>(fn_sig_str).map_err(|err| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "vespera: failed to parse stored signature for route '{}': {err}",
                    route_meta.path
                ),
            )
        })?;
        Ok(build_one(route_meta, &fn_sig)?.map(|(m, op)| (idx, m, op)))
    })
}

/// `filter_map` across worker threads for compile-time job fan-out.
///
/// Sequential below [`PARALLEL_THRESHOLD`] jobs (thread spawn overhead
/// dominates tiny projects); chunked `std::thread::scope` otherwise —
/// zero new dependencies.  `f` typically parses source *strings* with
/// `syn` and must return only plain `Send` data: proc-macro2 caches
/// "the compiler bridge works" in a global once it has been used on
/// the macro thread, and worker threads would then take the
/// real-bridge path and panic ("procedural macro API is used outside
/// of a procedural macro") — so the thread-safe fallback
/// implementation is forced for the duration of the parallel section.
/// Workers only ever create fallback tokens, so no compiler/fallback
/// token mixing can occur; the guard restores normal mode even if a
/// worker panics.
pub(super) fn parallel_filter_map<T: Sync, R: Send>(
    jobs: &[T],
    f: &(dyn Fn(&T) -> syn::Result<Option<R>> + Sync),
) -> syn::Result<Vec<R>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(jobs.len().div_ceil(PARALLEL_THRESHOLD));
    if workers <= 1 || jobs.len() < PARALLEL_THRESHOLD {
        return jobs.iter().filter_map(|job| f(job).transpose()).collect();
    }

    proc_macro2::fallback::force();
    let _guard = FallbackGuard;

    let chunk_size = jobs.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let results: syn::Result<Vec<R>> =
                            chunk.iter().filter_map(|job| f(job).transpose()).collect();
                        // Recursive generic components built on this worker
                        // travel back to the calling thread.
                        (results, generic_instances::take_materialized())
                    }))
                })
            })
            .collect();
        let mut results: Vec<R> = Vec::with_capacity(jobs.len());
        for handle in handles {
            let worker_result = handle
                .join()
                .map_err(|panic| worker_panic_error(panic.as_ref()))?;
            let (chunk_results, materialized) =
                worker_result.map_err(|panic| worker_panic_error(panic.as_ref()))?;
            generic_instances::merge_materialized(materialized);
            results.extend(chunk_results?);
        }
        Ok(results)
    })
}

fn worker_panic_error(panic: &(dyn std::any::Any + Send)) -> syn::Error {
    let message = panic.downcast_ref::<&str>().map_or_else(
        || {
            panic.downcast_ref::<String>().map_or_else(
                || "parallel macro worker panicked".to_string(),
                std::clone::Clone::clone,
            )
        },
        |message| (*message).to_string(),
    );
    syn::Error::new(
        proc_macro2::Span::call_site(),
        format!("vespera: parallel OpenAPI worker failed: {message}"),
    )
}

#[cfg(test)]
mod tests;
