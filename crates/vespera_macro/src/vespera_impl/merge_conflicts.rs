//! Overlaps between the routes one `vespera!` router serves: its own handlers,
//! its documentation routes (`docs_url` / `redoc_url` / `spec_url`) and the
//! routes of every merged child app (`merge = [...]`).
//!
//! axum panics on such an overlap when the router is built (or when
//! `Router::merge` runs), and the OpenAPI merge is first-wins, so the document
//! would silently describe only one of the two handlers.  Overlap means the
//! same `(method, path)` or one path shape spelled with different parameter
//! names (see `openapi_generator::RouteClaims`).  The check runs on every
//! `vespera!` expansion — cached or not, with or without a generated document
//! — and reads only the children's exported spec sidecars (an absent /
//! unreadable one is skipped here; the spec merge reports it when a document
//! is generated).

use syn::spanned::Spanned;

use super::{
    cache::{MergeSpecCache, MergeSpecRead},
    openapi_io::display_merge_path,
};
use crate::{
    metadata::CollectedMetadata,
    openapi_generator::{RouteClaims, RouteConflict},
};

/// The documentation routes a `vespera!` call adds: `(argument, url)`.
pub(super) fn doc_routes(
    input: &crate::router_codegen::ProcessedVesperaInput,
) -> Vec<(&'static str, &str)> {
    [
        ("docs_url", input.docs_url.as_deref()),
        ("redoc_url", input.redoc_url.as_deref()),
        ("spec_url", input.spec_url.as_deref()),
    ]
    .into_iter()
    .filter_map(|(key, url)| url.map(|url| (key, url)))
    .collect()
}

/// Return a compile error for the first route claimed twice across the app's
/// handlers, its documentation routes and its merged children.
pub(super) fn check_merge_route_conflicts(
    metadata: &CollectedMetadata,
    doc_routes: &[(&str, &str)],
    merge: &[syn::Path],
    merge_specs: &mut MergeSpecCache,
) -> syn::Result<()> {
    if merge.is_empty() && doc_routes.is_empty() {
        return Ok(());
    }
    let mut claims = RouteClaims::default();
    for route in &metadata.routes {
        // Already checked against each other (`check_duplicate_routes`).
        let _ = claims.claim(&route.method, &route.path, "this app");
    }
    for (key, url) in doc_routes {
        let owner = format!("`{key}`");
        claims.claim("get", url, &owner).map_err(|conflict| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "vespera! macro: {} — `{key}` adds a `GET {url}` route; move the \
                     documentation to an unused path",
                    describe(&conflict, &format!("GET {url}"), &owner)
                ),
            )
        })?;
    }
    for merge_path in merge {
        let Some((_, spec_file)) = merge_specs.spec_file_for(merge_path) else {
            continue;
        };
        let MergeSpecRead::Present(content) = merge_specs.read(&spec_file) else {
            continue;
        };
        let Ok(spec) = serde_json::from_str::<vespera_core::openapi::OpenApi>(content) else {
            continue;
        };
        let origin = format!("merged app `{}`", display_merge_path(merge_path));
        for (path, item) in &spec.paths {
            for (method, _) in item.operations() {
                claims
                    .claim(&method.to_string(), path, &origin)
                    .map_err(|conflict| {
                        syn::Error::new(
                            merge_path.span(),
                            format!(
                                "vespera! macro: {} — axum panics when merging overlapping \
                                 routes; remove one or give the child app a distinct \
                                 `export_app!(..., prefix = \"...\")`",
                                describe(&conflict, &format!("{method} {path}"), &origin)
                            ),
                        )
                    })?;
            }
        }
    }
    Ok(())
}

/// `route` (as `METHOD /path` or `/path`) claimed by `owner` against `conflict`.
fn describe(conflict: &RouteConflict, route: &str, owner: &str) -> String {
    match conflict {
        RouteConflict::Duplicate { previous, .. } => {
            format!("route `{route}` is served by both {previous} and {owner}")
        }
        RouteConflict::ParamNames {
            previous_path,
            previous,
        } => format!(
            "route `{route}` of {owner} and `{previous_path}` of {previous} are the same route \
             with different parameter names"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::metadata::RouteMetadata;

    fn route(method: &str, path: &str) -> RouteMetadata {
        serde_json::from_value(serde_json::json!({
            "method": method,
            "path": path,
            "function_name": "handler",
            "module_path": "routes::x",
            "file_path": "src/routes/x.rs",
        }))
        .unwrap()
    }

    fn spec(paths: &[(&str, &str)]) -> String {
        let mut doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": {"title": "child", "version": "1"},
            "paths": {}
        });
        for (method, path) in paths {
            doc["paths"][*path][*method] = serde_json::json!({"responses": {}});
        }
        doc.to_string()
    }

    fn run_with_docs(
        parent: &[(&str, &str)],
        docs: &[(&str, &str)],
        children: &[(&str, String)],
    ) -> Result<(), String> {
        let temp_dir = TempDir::new().unwrap();
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let vespera_dir =
            crate::vespera_impl::path_utils::find_target_dir(temp_dir.path()).join("vespera");
        fs::create_dir_all(&vespera_dir).unwrap();
        let mut merge = Vec::new();
        for (name, content) in children {
            fs::write(
                super::super::merge_lookup::child_spec_path(&vespera_dir, "child", name),
                content,
            )
            .unwrap();
            merge.push(syn::parse_str::<syn::Path>(&format!("child::{name}")).unwrap());
        }
        let mut metadata = CollectedMetadata::new();
        metadata.routes = parent.iter().map(|(m, p)| route(m, p)).collect();
        let mut merge_specs = MergeSpecCache::new();
        check_merge_route_conflicts(&metadata, docs, &merge, &mut merge_specs)
            .map_err(|e| e.to_string())
    }

    fn run(parent: &[(&str, &str)], children: &[(&str, String)]) -> Result<(), String> {
        run_with_docs(parent, &[], children)
    }

    /// One path shape with different parameter names conflicts in axum
    /// whatever the methods are — across the parent and a child, too.
    #[rstest::rstest]
    #[case::parent_and_child(
        "merge_conflict_param_names_parent_child",
        &[("get", "/users/{id}")],
        &[("delete", "/users/{user_id}")],
    )]
    #[case::child_against_itself_is_its_own_problem(
        "merge_conflict_param_names_child_child",
        &[],
        &[("get", "/items/{a}"), ("put", "/items/{b}")],
    )]
    fn param_name_overlaps_conflict(
        #[case] name: &str,
        #[case] parent: &[(&str, &str)],
        #[case] child: &[(&str, &str)],
    ) {
        let err = run(parent, &[("UsersApp", spec(child))]).unwrap_err();
        insta::assert_snapshot!(name, err);
    }

    /// `docs_url` / `redoc_url` / `spec_url` add GET routes of their own: one
    /// shadowing an app route or a merged child's route is a compile error.
    #[rstest::rstest]
    #[case::app_route(
        "doc_route_conflict_app",
        &[("get", "/docs")],
        &[("docs_url", "/docs")],
        &[],
    )]
    #[case::second_doc_param_names(
        "doc_route_conflict_param_names",
        &[("get", "/api/{id}")],
        &[("spec_url", "/api/{name}")],
        &[],
    )]
    #[case::merged_child(
        "doc_route_conflict_child",
        &[],
        &[("redoc_url", "/redoc")],
        &[("get", "/redoc")],
    )]
    fn doc_routes_conflict(
        #[case] name: &str,
        #[case] parent: &[(&str, &str)],
        #[case] docs: &[(&str, &str)],
        #[case] child: &[(&str, &str)],
    ) {
        let err = run_with_docs(parent, docs, &[("UsersApp", spec(child))]).unwrap_err();
        insta::assert_snapshot!(name, err);
    }

    /// A doc route on a different method/path is fine, and so is a doc route
    /// on an app without merged children.
    #[test]
    fn distinct_doc_routes_pass() {
        run_with_docs(
            &[("post", "/docs"), ("get", "/users")],
            &[("docs_url", "/docs"), ("spec_url", "/openapi.json")],
            &[],
        )
        .unwrap();
        let input = crate::router_codegen::ProcessedVesperaInput {
            docs_url: Some("/docs".to_string()),
            spec_url: Some("/openapi.json".to_string()),
            ..Default::default()
        };
        assert_eq!(
            doc_routes(&input),
            [("docs_url", "/docs"), ("spec_url", "/openapi.json")]
        );
    }

    #[test]
    fn parent_and_child_on_one_route_conflict() {
        let err = run(
            &[("get", "/users/{id}")],
            &[("UsersApp", spec(&[("get", "/users/{id}")]))],
        )
        .unwrap_err();
        insta::assert_snapshot!("merge_conflict_parent_child", err);
    }

    #[test]
    fn two_children_on_one_route_conflict() {
        let err = run(
            &[],
            &[
                ("AApp", spec(&[("post", "/items")])),
                ("BApp", spec(&[("post", "/items")])),
            ],
        )
        .unwrap_err();
        insta::assert_snapshot!("merge_conflict_two_children", err);
    }

    #[test]
    fn distinct_methods_and_unreadable_specs_pass() {
        run(
            &[("get", "/users")],
            &[
                ("UsersApp", spec(&[("post", "/users")])),
                ("BrokenApp", "{ not json".to_string()),
            ],
        )
        .unwrap();
        // A child whose sidecar is missing is skipped as well, and so is every
        // child when no sidecar directory can be located.
        let metadata = CollectedMetadata::new();
        let missing = [syn::parse_str::<syn::Path>("child::Missing").unwrap()];
        let temp_dir = TempDir::new().unwrap();
        {
            let _manifest =
                crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
            check_merge_route_conflicts(&metadata, &[], &missing, &mut MergeSpecCache::new())
                .unwrap();
        }
        let _manifest = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        check_merge_route_conflicts(&metadata, &[], &missing, &mut MergeSpecCache::new()).unwrap();
        check_merge_route_conflicts(&metadata, &[], &[], &mut MergeSpecCache::new()).unwrap();
    }
}
