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
    collector::normalize_path_key,
    metadata::CollectedMetadata,
    parser::{OperationRouteConfig, build_operation_from_function},
    route_impl::StoredRouteInfo,
};

type FnIndex<'a> = HashMap<&'a str, HashMap<String, &'a syn::ItemFn>>;
type StorageFnSigs<'a> = HashMap<(Option<String>, &'a str), Option<&'a str>>;

/// Build path items and collect tags from route metadata.
///
/// Uses `route_storage` (from `#[route]` macro) as the primary source for function
/// signatures. Falls back to pre-built `file_cache` when ROUTE_STORAGE doesn't
/// have an entry (e.g., during tests or for routes added without the attribute).
pub(super) fn build_path_items(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
    file_cache: &HashMap<String, syn::File>,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<(BTreeMap<String, PathItem>, BTreeSet<String>)> {
    let mut paths = BTreeMap::new();
    let mut all_tags = BTreeSet::new();

    // Build the file-AST function index FIRST so the storage path
    // below can skip any function whose AST is already reachable through
    // `file_cache`.  `collector::collect_metadata` has already walked
    // these files via `syn::parse_file`, so re-parsing `fn_sig_str`
    // from ROUTE_STORAGE for the same function is pure duplicated work.
    let fn_index: HashMap<&str, HashMap<String, &syn::ItemFn>> = file_cache
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
            (path.as_str(), fns)
        })
        .collect();

    // ROUTE_STORAGE-backed function signatures (skipped when the same
    // function is already covered by `fn_index` — re-parsing would be
    // duplicated work).  These are plain *strings*, so the expensive
    // `syn::parse_str` + operation build runs on worker threads below;
    // `syn` ASTs are not `Send`, which is also why fn_index-backed
    // routes stay on this thread.
    let cwd = std::env::current_dir().unwrap_or_default();
    let storage_fn_sigs = build_storage_fn_sigs(route_storage, &fn_index, &cwd);

    // Split routes by signature source. `idx` preserves the original
    // route order so PathItem operations are applied deterministically
    // regardless of which thread produced them.
    let mut parallel_jobs: Vec<(usize, &crate::metadata::RouteMetadata, &str)> = Vec::new();
    let mut ast_jobs: Vec<(usize, &crate::metadata::RouteMetadata, &syn::Signature)> = Vec::new();
    for (idx, route_meta) in metadata.routes.iter().enumerate() {
        // ROUTE_STORAGE first (avoids file_cache dependency for known
        // routes) — same priority order as the previous sequential code.
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
        } else if let Some(fns) = fn_index.get(route_meta.file_path.as_str())
            && let Some(fn_item) = fns.get(&route_meta.function_name)
        {
            ast_jobs.push((idx, route_meta, &fn_item.sig));
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

    for (idx, route_meta, fn_sig) in ast_jobs {
        if let Some((method, operation)) = build_one(route_meta, fn_sig)? {
            results.push((idx, method, operation));
        }
    }

    // Deterministic assembly in original route order.
    results.sort_unstable_by_key(|(idx, _, _)| *idx);
    for (idx, method, operation) in results {
        let route_meta = &metadata.routes[idx];
        if let Some(tags) = &route_meta.tags {
            for tag in tags {
                all_tags.insert(tag.clone());
            }
        }
        let path_item = paths
            .entry(route_meta.path.clone())
            .or_insert_with(PathItem::default);
        path_item.set_operation(method, operation);
    }

    Ok((paths, all_tags))
}

fn build_storage_fn_sigs<'a>(
    route_storage: &'a [StoredRouteInfo],
    fn_index: &FnIndex<'_>,
    cwd: &std::path::Path,
) -> StorageFnSigs<'a> {
    let mut storage = HashMap::with_capacity(route_storage.len());
    for s in route_storage {
        let already_in_ast = s
            .file_path
            .as_deref()
            .and_then(|fp| fn_index.get(fp))
            .is_some_and(|fns| fns.contains_key(&s.fn_name));
        if already_in_ast {
            continue;
        }
        let key = (
            s.file_path
                .as_deref()
                .map(|path| normalize_path_key(path, cwd)),
            s.fn_name.as_str(),
        );
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
                        chunk.iter().filter_map(|job| f(job).transpose()).collect()
                    }))
                })
            })
            .collect();
        let mut results: Vec<R> = Vec::with_capacity(jobs.len());
        for handle in handles {
            let worker_result = handle
                .join()
                .map_err(|panic| worker_panic_error(panic.as_ref()))?;
            let chunk_results: syn::Result<Vec<R>> =
                worker_result.map_err(|panic| worker_panic_error(panic.as_ref()))?;
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
mod tests {
    use std::{collections::HashMap, fs, path::PathBuf};

    use rstest::rstest;
    use tempfile::TempDir;

    use crate::{
        metadata::{CollectedMetadata, RouteMetadata, StructMetadata},
        openapi_generator::generate_openapi_doc_with_metadata,
        route_impl::StoredRouteInfo,
    };

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.path().join(filename);
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    /// Build a `RouteMetadata` with the boilerplate-heavy fields defaulted.
    fn route_meta(method: &str, path: &str, fn_name: &str, file_path: &str) -> RouteMetadata {
        RouteMetadata {
            method: method.to_string(),
            path: path.to_string(),
            function_name: fn_name.to_string(),
            module_path: format!("test::{fn_name}"),
            file_path: file_path.to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        }
    }

    #[test]
    fn route_in_file_cache_appears_in_paths() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> String { \"users\".to_string() }",
        );
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_meta(
            "GET",
            "/users",
            "get_users",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        let op = doc
            .paths
            .get("/users")
            .and_then(|p| p.get.as_ref())
            .expect("GET op");
        assert_eq!(op.operation_id.as_deref(), Some("get_users"));
    }

    #[test]
    fn route_storage_dedup_skips_already_in_ast() {
        // When a route's `fn_sig_str` was already discovered by parsing the
        // source file via `file_cache`, the storage-parse step must skip
        // re-parsing it — exercises the `already_in_ast → return None`
        // branch inside `route_fn_cache` construction.
        let route_file_path = "/virtual/users.rs".to_string();
        let route_src = "pub fn get_users() -> String { \"users\".to_string() }";
        let parsed: syn::File = syn::parse_str(route_src).expect("route src parses");
        let mut file_cache: HashMap<String, syn::File> = HashMap::new();
        file_cache.insert(route_file_path.clone(), parsed);

        let mut metadata = CollectedMetadata::new();
        metadata
            .routes
            .push(route_meta("GET", "/users", "get_users", &route_file_path));

        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            file_path: Some(route_file_path),
            fn_sig_str: route_src.to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            Some(file_cache),
            &route_storage,
        );

        let op = doc
            .paths
            .get("/users")
            .and_then(|p| p.get.as_ref())
            .expect("GET op");
        assert_eq!(op.operation_id.as_deref(), Some("get_users"));
    }

    #[test]
    fn route_storage_fast_path_when_fn_not_in_file_cache() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> String { \"users\".to_string() }\n",
        );
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_meta(
            "GET",
            "/users",
            "get_users",
            &route_file.to_string_lossy(),
        ));
        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: "fn get_users() -> String".to_string(),
            file_path: None,
        }];

        let doc = generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            None,
            &route_storage,
        );

        let op = doc
            .paths
            .get("/users")
            .and_then(|p| p.get.as_ref())
            .expect("GET op");
        assert_eq!(op.operation_id.as_deref(), Some("get_users"));
    }

    #[test]
    fn route_storage_fast_path_disambiguates_same_fn_name_by_file_path() {
        let users_path = "/virtual/users.rs".to_string();
        let posts_path = "/virtual/posts.rs".to_string();
        let mut metadata = CollectedMetadata::new();
        metadata
            .routes
            .push(route_meta("GET", "/users", "list", &users_path));
        metadata
            .routes
            .push(route_meta("GET", "/posts", "list", &posts_path));

        let route_storage = vec![
            StoredRouteInfo {
                fn_name: "list".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: None,
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: "fn list() -> String".to_string(),
                file_path: Some(users_path),
            },
            StoredRouteInfo {
                fn_name: "list".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: None,
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: "fn list() -> i32".to_string(),
                file_path: Some(posts_path),
            },
        ];

        let doc = generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            None,
            &route_storage,
        );

        let users_schema = doc
            .paths
            .get("/users")
            .and_then(|path| path.get.as_ref())
            .and_then(|op| op.responses.get("200"))
            .and_then(|response| response.content.as_ref())
            .and_then(|content| content.values().next())
            .and_then(|media| media.schema.as_ref())
            .expect("users response schema");
        let posts_schema = doc
            .paths
            .get("/posts")
            .and_then(|path| path.get.as_ref())
            .and_then(|op| op.responses.get("200"))
            .and_then(|response| response.content.as_ref())
            .and_then(|content| content.values().next())
            .and_then(|media| media.schema.as_ref())
            .expect("posts response schema");

        let schema_type = |schema: &vespera_core::schema::SchemaRef| match schema {
            vespera_core::schema::SchemaRef::Inline(schema) => schema.schema_type,
            vespera_core::schema::SchemaRef::Ref(reference) => {
                panic!("expected inline schema, got {}", reference.ref_path)
            }
        };
        assert_eq!(
            schema_type(users_schema),
            Some(vespera_core::schema::SchemaType::String)
        );
        assert_eq!(
            schema_type(posts_schema),
            Some(vespera_core::schema::SchemaType::Integer)
        );
    }

    #[test]
    fn route_storage_legacy_none_file_path_is_skipped_when_ambiguous() {
        let users_path = "/virtual/users.rs".to_string();
        let posts_path = "/virtual/posts.rs".to_string();
        let mut metadata = CollectedMetadata::new();
        metadata
            .routes
            .push(route_meta("GET", "/users", "list", &users_path));
        metadata
            .routes
            .push(route_meta("GET", "/posts", "list", &posts_path));

        let mut file_cache = HashMap::new();
        file_cache.insert(
            users_path.clone(),
            syn::parse_str("pub fn list() -> String { String::new() }").unwrap(),
        );
        file_cache.insert(
            posts_path.clone(),
            syn::parse_str("pub fn list() -> i32 { 1 }").unwrap(),
        );

        let route_storage = vec![
            StoredRouteInfo {
                fn_name: "list".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: None,
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: "fn list() -> bool".to_string(),
                file_path: None,
            },
            StoredRouteInfo {
                fn_name: "list".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: None,
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: "fn list() -> bool".to_string(),
                file_path: None,
            },
        ];

        let doc = generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            Some(file_cache),
            &route_storage,
        );

        let response_schema_type = |path: &str| {
            let schema = doc
                .paths
                .get(path)
                .and_then(|path| path.get.as_ref())
                .and_then(|op| op.responses.get("200"))
                .and_then(|response| response.content.as_ref())
                .and_then(|content| content.values().next())
                .and_then(|media| media.schema.as_ref())
                .expect("response schema");
            match schema {
                vespera_core::schema::SchemaRef::Inline(schema) => schema.schema_type,
                vespera_core::schema::SchemaRef::Ref(reference) => {
                    panic!("expected inline schema, got {}", reference.ref_path)
                }
            }
        };

        assert_eq!(
            response_schema_type("/users"),
            Some(vespera_core::schema::SchemaType::String)
        );
        assert_eq!(
            response_schema_type("/posts"),
            Some(vespera_core::schema::SchemaType::Integer)
        );
    }

    #[test]
    fn route_with_function_not_in_ast_is_skipped() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_items() -> String { \"items\".to_string() }\n",
        );
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_meta(
            "GET",
            "/users",
            "get_users",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert!(
            doc.paths.is_empty(),
            "Route with non-matching function should be skipped"
        );
    }

    #[test]
    fn route_and_struct_appear_together() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "user_route.rs",
            r#"
use crate::user::User;

pub fn get_user() -> User {
User { id: 1, name: "Alice".to_string() }
}
"#,
        );

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "User".to_string(),
            definition: "struct User { id: i32, name: String }".to_string(),
            ..Default::default()
        });
        metadata.routes.push(route_meta(
            "GET",
            "/user",
            "get_user",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(
            Some("Test API".to_string()),
            Some("1.0.0".to_string()),
            None,
            None,
            &metadata,
            None,
            &[],
        );

        let schemas = doc
            .components
            .as_ref()
            .and_then(|c| c.schemas.as_ref())
            .expect("schemas present");
        assert!(schemas.contains_key("User"));
        assert!(
            doc.paths
                .get("/user")
                .and_then(|p| p.get.as_ref())
                .is_some()
        );
    }

    #[test]
    fn multiple_methods_share_path_item() {
        let temp_dir = TempDir::new().unwrap();
        let r1 = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> String { \"users\".to_string() }",
        );
        let r2 = create_temp_file(
            &temp_dir,
            "create_user.rs",
            "pub fn create_user() -> String { \"created\".to_string() }",
        );

        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_meta(
            "GET",
            "/users",
            "get_users",
            &r1.to_string_lossy(),
        ));
        metadata.routes.push(route_meta(
            "POST",
            "/users",
            "create_user",
            &r2.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert_eq!(doc.paths.len(), 1);
        let path_item = doc.paths.get("/users").unwrap();
        assert!(path_item.get.is_some());
        assert!(path_item.post.is_some());
    }

    #[test]
    fn tags_and_description_propagate_to_operation() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> String { \"users\".to_string() }",
        );

        let mut metadata = CollectedMetadata::new();
        let mut rm = route_meta("GET", "/users", "get_users", &route_file.to_string_lossy());
        rm.error_status = Some(vec![404]);
        rm.tags = Some(vec!["users".to_string(), "admin".to_string()]);
        rm.description = Some("Get all users".to_string());
        metadata.routes.push(rm);

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        let op = doc
            .paths
            .get("/users")
            .and_then(|p| p.get.as_ref())
            .unwrap();
        assert_eq!(op.description.as_deref(), Some("Get all users"));
        let tags = doc.tags.as_ref().expect("tags present");
        assert!(tags.iter().any(|t| t.name == "users"));
        assert!(tags.iter().any(|t| t.name == "admin"));
    }

    /// File-read / parse failures must not produce phantom routes or schemas.
    #[rstest]
    #[case::route_file_read_failure("/nonexistent/route.rs", None)]
    #[case::route_file_parse_failure("", Some("invalid rust syntax {"))]
    fn file_errors_skip_route(
        #[case] file_path_template: &str,
        #[case] write_invalid: Option<&str>,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let final_file_path = write_invalid.map_or_else(
            || file_path_template.to_string(),
            |content| {
                create_temp_file(&temp_dir, "invalid_route.rs", content)
                    .to_string_lossy()
                    .to_string()
            },
        );

        let mut metadata = CollectedMetadata::new();
        metadata
            .routes
            .push(route_meta("GET", "/users", "get_users", &final_file_path));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert!(!doc.paths.contains_key("/users"));
        // schemas must also be empty — no struct was registered.
        if let Some(schemas) = doc.components.as_ref().and_then(|c| c.schemas.as_ref()) {
            assert!(!schemas.contains_key("User"));
        }
    }

    #[test]
    fn unknown_http_method_route_is_compile_error() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> String { \"users\".to_string() }",
        );

        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_meta(
            "INVALID",
            "/users",
            "get_users",
            &route_file.to_string_lossy(),
        ));

        let err = crate::openapi_generator::try_generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            None,
            &[],
        )
        .expect_err("unknown method should fail OpenAPI generation");

        assert!(err.to_string().contains("unsupported HTTP method"));
    }

    #[test]
    fn unknown_method_fails_even_when_valid_route_exists() {
        let temp_dir = TempDir::new().unwrap();
        let route_file = create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
pub fn get_users() -> String
{ "users".to_string() }

pub fn create_users() -> String { "created".to_string() }
"#,
        );
        let file_path = route_file.to_string_lossy().to_string();

        let mut metadata = CollectedMetadata::new();
        metadata
            .routes
            .push(route_meta("CONNECT", "/users", "get_users", &file_path));
        metadata
            .routes
            .push(route_meta("POST", "/users", "create_users", &file_path));

        let err = crate::openapi_generator::try_generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            None,
            &[],
        )
        .expect_err("unknown method should fail OpenAPI generation");

        assert!(err.to_string().contains("unsupported HTTP method"));
    }
}
