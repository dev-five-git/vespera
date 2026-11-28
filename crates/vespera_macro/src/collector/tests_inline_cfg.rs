//! Pins the two documented `cfg` behaviours of the inline-module rule
//! (`module_scope`, round-44 MR44-0-L1 / R44-MR1-L1): a cfg-gated route file
//! skips its inline-module handlers instead of rejecting them, while a handler
//! with its own `#[cfg]` inside an inline module is rejected in every build.

use std::fs;

use tempfile::TempDir;

use super::*;

fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.path().join(filename);
    fs::write(&file_path, content).expect("Failed to write temp file");
    file_path
}

/// Slow path of a route file behind a module declaration's `#[cfg]`: its
/// top-level handler is recorded (gated), its inline-module handler is skipped
/// without the inline-module compile error an ungated file gets.
#[test]
fn inline_module_route_in_a_cfg_gated_file_is_skipped_not_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "#[cfg(feature = \"admin\")]\npub mod panel;\n",
    );
    create_temp_file(
        &temp_dir,
        "panel.rs",
        "#[route(get)] pub async fn top() {}\nmod inner { #[route(get)] pub async fn probe() {} }\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[])
        .expect("a cfg-gated inline-module handler is not a compile error");

    assert!(metadata.routes.is_empty());
    let gated: Vec<&str> = metadata
        .cfg_routes
        .iter()
        .map(|route| route.function_name.as_str())
        .collect();
    assert_eq!(gated, ["top"]);
}

/// A handler with its own `#[cfg(..)]` inside an inline module is rejected
/// although the feature it names is off in this build: the rule is
/// source-shape based and never evaluates features.
#[test]
fn inline_module_handler_with_its_own_cfg_is_rejected_when_the_cfg_is_off() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "users.rs",
        "mod v2 { #[cfg(feature = \"beta\")] #[route(get)] pub async fn b() {} }\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let error = collect_metadata(temp_dir.path(), "routes", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("`b` is declared inside the inline module `mod v2 { .. }`"),
        "{error}"
    );
}
