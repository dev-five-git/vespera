//! Collector tests for a route file's own inner `#![cfg(..)]` (round-41
//! R41-MR0-M1): the file's gate applies to every route in it, as a
//! declaration's `#[cfg(..)]` does.

use std::fs;

use tempfile::TempDir;

use super::*;

fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.path().join(filename);
    fs::write(&file_path, content).expect("Failed to write temp file");
    file_path
}

/// Slow path (no `#[route]` storage entry): a route file whose inner
/// `#![cfg(feature = "admin")]` gates it is mounted only under that predicate,
/// never as an ungated route the router would reference unconditionally.
#[test]
fn slow_path_inner_cfg_gates_the_route_file() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(&temp_dir, "mod.rs", "pub mod panel;\n");
    create_temp_file(
        &temp_dir,
        "panel.rs",
        "#![cfg(feature = \"admin\")]\n#[route(get)] pub async fn panel() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    assert!(metadata.routes.is_empty());
    let gated: Vec<(&str, &[String])> = metadata
        .cfg_routes
        .iter()
        .map(|route| (route.function_name.as_str(), route.cfg.as_slice()))
        .collect();
    assert_eq!(gated, [("panel", &["feature = \"admin\"".to_string()][..])]);
}

/// Fast path (this build registered the route): a test build's
/// `#![cfg(test)]` route file is test-only as a whole, so the route is
/// recorded as test-only and kept out of the document, as a `#[cfg(test)]`
/// module's routes are.
#[test]
fn fast_path_inner_test_cfg_makes_the_route_file_test_only() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file = create_temp_file(
        &temp_dir,
        "probe.rs",
        "#![cfg(test)]\n#[route(get)] pub async fn probe() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();
    let file_path = file.display().to_string();
    let storage = [StoredRouteInfo {
        own_test_only: false,
        fn_name: "probe".to_string(),
        method: Some("get".to_string()),
        fn_sig_str: "async fn probe()".to_string(),
        file_path: Some(file_path),
        ..StoredRouteInfo::default()
    }];

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &storage).unwrap();

    let names: Vec<&str> = metadata
        .test_only_routes
        .iter()
        .map(|route| route.function_name.as_str())
        .collect();
    assert_eq!(names, ["probe"]);
}
