//! Collector tests for twin `mod` declarations that load one route folder file
//! under more than one name (round-37 MR0-M1).

use std::fs;

use tempfile::TempDir;

use super::*;

fn create_temp_file(dir: &TempDir, filename: &str, content: &str) {
    let file_path = dir.path().join(filename);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create parent directory");
    }
    fs::write(&file_path, content).expect("Failed to write temp file");
}

/// A route-free helper loaded by two ungated `mod` declarations (`pub mod
/// shared;` beside a `#[path]` alias) is no conflict: the folder still
/// collects, and the route beside it keeps its call path (round-37 MR0-M1).
#[test]
fn ungated_twin_helper_file_does_not_fail_collection() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "pub mod shared;\n#[path = \"shared.rs\"]\npub mod shared_alias;\npub mod users;\n",
    );
    create_temp_file(&temp_dir, "shared.rs", "pub fn helper() {}\n");
    create_temp_file(
        &temp_dir,
        "users.rs",
        "#[route(get)] pub async fn list() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    let modules: Vec<&str> = metadata
        .routes
        .iter()
        .map(|route| route.module_path.as_str())
        .collect();
    assert_eq!(modules, ["routes::users"]);
}

/// The same helper pair declared with the alias first: the ungated twins keep
/// the first declared name, so the alias (declared first) is the call path
/// (round-37 MR0-M1).
#[test]
fn ungated_twin_helper_file_keeps_the_first_declared_name() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "#[path = \"shared.rs\"]\npub mod shared_alias;\npub mod shared;\n",
    );
    create_temp_file(
        &temp_dir,
        "shared.rs",
        "#[route(get)] pub async fn list() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    let modules: Vec<&str> = metadata
        .routes
        .iter()
        .map(|route| route.module_path.as_str())
        .collect();
    assert_eq!(modules, ["routes::shared_alias"]);
}
