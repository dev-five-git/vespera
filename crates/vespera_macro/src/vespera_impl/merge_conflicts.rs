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
//!
//! Known limit: a merged child's `#[cfg]`-gated routes are not in its exported
//! sidecar (only its ungated routes are), so they are not claimed here. A
//! child feature that mounts a route overlapping the parent's panics when the
//! merged router is built instead of failing the expansion.

use syn::spanned::Spanned;

use super::{
    cache::{MergeSpecCache, MergeSpecRead},
    merge_lookup::{axum_path, catch_all_markers},
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
///
/// On success, the `#[cfg(..)] compile_error!` statements for `#[cfg]`-gated
/// routes that overlap a merged child's route: axum panics on the merge
/// whenever such a route is compiled in (an overlap with the app's own or
/// documentation routes is reported by the router generator instead).
pub(super) fn check_merge_route_conflicts(
    metadata: &CollectedMetadata,
    doc_routes: &[(&str, &str)],
    merge: &[syn::Path],
    merge_specs: &mut MergeSpecCache,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    if merge.is_empty() && doc_routes.is_empty() {
        return Ok(Vec::new());
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
    let own = claims.clone();
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
        // A spec with the catch-all marker says exactly which path-final
        // parameters are catch-alls; an older one is claimed leniently.
        let markers = catch_all_markers(content);
        for (path, item) in &spec.paths {
            for (method, _) in item.operations() {
                let method = method.to_string();
                let claimed = match &markers {
                    Some(catch_alls) => {
                        claims.claim(&method, &axum_path(path, catch_alls), &origin)
                    }
                    None => claims.claim_template(&method, path, &origin),
                };
                claimed.map_err(|conflict| {
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
    Ok(gated_child_conflicts(metadata, &own, &claims))
}

/// `#[cfg(all(..))] compile_error!(..);` for every gated route that `own`
/// (the app's ungated and documentation routes) accepts but `all` (those plus
/// the merged children's routes) rejects. Only the app's own gated routes are
/// checked: a merged child's gated routes are outside `all` (see the module
/// doc's known limit).
fn gated_child_conflicts(
    metadata: &CollectedMetadata,
    own: &RouteClaims,
    all: &RouteClaims,
) -> Vec<proc_macro2::TokenStream> {
    metadata
        .cfg_routes
        .iter()
        .filter(|route| own.clone().claim(&route.method, &route.path, "").is_ok())
        .filter_map(|route| {
            let handler = if route.module_path.is_empty() {
                route.function_name.clone()
            } else {
                format!("{}::{}", route.module_path, route.function_name)
            };
            let owner = format!("`{handler}` (`#[cfg]`-gated route)");
            let conflict = all
                .clone()
                .claim(&route.method, &route.path, &owner)
                .err()?;
            let route_name = format!("{} {}", route.method.to_ascii_uppercase(), route.path);
            let message = format!(
                "vespera! macro: {} — axum panics when merging overlapping routes; remove one \
                 or give the child app a distinct `export_app!(..., prefix = \"...\")`",
                describe(&conflict, &route_name, &owner)
            );
            let predicates = route.cfg.iter().map(|predicate| {
                predicate
                    .parse::<proc_macro2::TokenStream>()
                    .unwrap_or_default()
            });
            Some(quote::quote!(#[cfg(all(#(#predicates),*))] ::core::compile_error!(#message);))
        })
        .collect()
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
        RouteConflict::WildcardKind {
            previous_path,
            previous,
        } => format!(
            "route `{route}` of {owner} and `{previous_path}` of {previous} put a parameter \
             and a catch-all at the same position"
        ),
        RouteConflict::CatchAllNotLast => {
            format!("route `{route}` of {owner} continues after a catch-all")
        }
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
            .map(drop)
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
    #[case::catch_all_vs_child_param(
        "doc_route_conflict_wildcard_kind",
        &[],
        &[("docs_url", "/d/{*rest}")],
        &[("get", "/d/{id}/raw")],
    )]
    #[case::text_after_catch_all(
        "doc_route_conflict_catch_all_not_last",
        &[],
        &[("docs_url", "/d/{*rest}/raw")],
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

    /// A child's spec template drops the `*` of a catch-all, so its path-final
    /// `{rest}` next to the parent's `{*rest}` is not a wildcard-kind conflict.
    #[test]
    fn child_template_catch_all_matches_parent_catch_all() {
        run(
            &[("get", "/files/{*rest}")],
            &[("FilesApp", spec(&[("post", "/files/{rest}")]))],
        )
        .unwrap();
    }

    /// A spec of the same shape with the catch-all marker (`catch_alls`: the
    /// template → catch-all name pairs it lists).
    fn spec_with_catch_alls(paths: &[(&str, &str)], catch_alls: &[(&str, &str)]) -> String {
        let mut doc: serde_json::Value = serde_json::from_str(&spec(paths)).unwrap();
        doc[super::super::merge_lookup::CATCH_ALL_KEY] = catch_alls
            .iter()
            .map(|(template, name)| (template.to_string(), serde_json::json!(name)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        doc.to_string()
    }

    /// With the marker a child's plain `{rest}` is a parameter: next to the
    /// parent's `{*rest}` it is the conflict axum would panic on at merge.
    #[test]
    fn marked_plain_child_param_conflicts_with_parent_catch_all() {
        let err = run(
            &[("get", "/files/{*rest}")],
            &[(
                "FilesApp",
                spec_with_catch_alls(&[("post", "/files/{rest}")], &[]),
            )],
        )
        .unwrap_err();
        assert!(err.contains("catch-all"), "{err}");
    }

    /// The marked catch-all of a child meets the parent's catch-all: no
    /// conflict (the same position, the same kind, the same name).
    #[test]
    fn marked_child_catch_all_matches_parent_catch_all() {
        run(
            &[("get", "/files/{*rest}")],
            &[(
                "FilesApp",
                spec_with_catch_alls(&[("post", "/files/{rest}")], &[("/files/{rest}", "rest")]),
            )],
        )
        .unwrap();
    }

    /// A marked child catch-all next to the parent's plain `{id}` is the
    /// conflict axum panics on, which the unmarked (lenient) read let through.
    #[test]
    fn marked_child_catch_all_conflicts_with_parent_param() {
        let err = run(
            &[("get", "/files/{id}")],
            &[(
                "FilesApp",
                spec_with_catch_alls(&[("post", "/files/{rest}")], &[("/files/{rest}", "rest")]),
            )],
        )
        .unwrap_err();
        assert!(err.contains("catch-all"), "{err}");
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

    /// A `#[cfg]`-gated route on a merged child's route is a
    /// `compile_error!` under the route's own `#[cfg]`; one that overlaps the
    /// app's own routes is left to the router generator, and a distinct one
    /// passes.
    #[test]
    fn gated_routes_are_checked_against_merged_children() {
        let temp_dir = TempDir::new().unwrap();
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let vespera_dir =
            crate::vespera_impl::path_utils::find_target_dir(temp_dir.path()).join("vespera");
        fs::create_dir_all(&vespera_dir).unwrap();
        fs::write(
            super::super::merge_lookup::child_spec_path(&vespera_dir, "child", "OpsApp"),
            spec(&[("get", "/health")]),
        )
        .unwrap();
        let merge = [syn::parse_str::<syn::Path>("child::OpsApp").unwrap()];
        let gated = |path: &str, module_path: &str| crate::metadata::CfgRoute {
            cfg: vec!["feature = \"ops\"".to_owned()],
            method: "get".to_owned(),
            path: path.to_owned(),
            module_path: module_path.to_owned(),
            function_name: "health".to_owned(),
            file_path: "src/routes/ops.rs".to_owned(),
            deprecated: false,
        };
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route("get", "/users"));
        metadata.cfg_routes = vec![
            gated("/health", "routes::ops"),
            gated("/users", "routes::ops"),
            gated("/other", ""),
        ];
        let errors =
            check_merge_route_conflicts(&metadata, &[], &merge, &mut MergeSpecCache::new())
                .unwrap();
        let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("# [cfg (all (feature = \"ops\"))] :: core :: compile_error !"),
            "{}",
            errors[0]
        );
        assert!(
            errors[0].contains(
                "route `GET /health` is served by both merged app `child::OpsApp` and `routes::ops::health` (`#[cfg]`-gated route)"
            ),
            "{}",
            errors[0]
        );
    }
}
