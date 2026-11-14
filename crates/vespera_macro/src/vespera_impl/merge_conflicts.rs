//! `merge = [...]`: reject a `(method, path)` served by both the parent app
//! and a merged child app (or by two merged children).
//!
//! axum panics on such an overlap when `Router::merge` runs, and the OpenAPI
//! merge is first-wins, so the document would silently describe only one of
//! the two handlers.  The check runs on every `vespera!` expansion — cached or
//! not, with or without a generated document — and reads only the children's
//! exported spec sidecars (an absent / unreadable one is skipped here; the
//! spec merge reports it when a document is generated).

use std::collections::HashMap;

use syn::spanned::Spanned;

use super::{
    cache::{MergeSpecCache, MergeSpecRead},
    openapi_io::display_merge_path,
};
use crate::{metadata::CollectedMetadata, parser::openapi_path_template};

/// Return a compile error for the first route claimed twice across the
/// parent app and its merged children.
pub(super) fn check_merge_route_conflicts(
    metadata: &CollectedMetadata,
    merge: &[syn::Path],
    merge_specs: &mut MergeSpecCache,
) -> syn::Result<()> {
    if merge.is_empty() {
        return Ok(());
    }
    let mut claimed: HashMap<(String, String), String> = HashMap::new();
    for route in &metadata.routes {
        claimed
            .entry((
                route.method.to_ascii_uppercase(),
                openapi_path_template(&route.path),
            ))
            .or_insert_with(|| "this app".to_string());
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
                let key = (method.to_string(), path.clone());
                if let Some(previous) = claimed.get(&key) {
                    return Err(syn::Error::new(
                        merge_path.span(),
                        format!(
                            "vespera! macro: route `{} {}` is served by both {previous} and \
                             {origin} — axum panics when merging overlapping routes; remove \
                             one or give the child app a distinct `export_app!(..., prefix = \
                             \"...\")`",
                            key.0, key.1
                        ),
                    ));
                }
                claimed.insert(key, origin.clone());
            }
        }
    }
    Ok(())
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

    fn run(parent: &[(&str, &str)], children: &[(&str, String)]) -> Result<(), String> {
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
        check_merge_route_conflicts(&metadata, &merge, &mut merge_specs).map_err(|e| e.to_string())
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
            check_merge_route_conflicts(&metadata, &missing, &mut MergeSpecCache::new()).unwrap();
        }
        let _manifest = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        check_merge_route_conflicts(&metadata, &missing, &mut MergeSpecCache::new()).unwrap();
        check_merge_route_conflicts(&metadata, &[], &mut MergeSpecCache::new()).unwrap();
    }
}
