//! Orchestrator checks that must hold whether or not a spec is generated.

use std::fs;

use tempfile::TempDir;

use super::*;

fn spec_less(folder: &str) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: folder.to_string(),
        ..ProcessedVesperaInput::default()
    }
}

/// Two handlers on one `(method, path)` are a compile error even when no
/// OpenAPI document is generated — axum would panic building the router.
#[test]
fn duplicate_route_is_rejected_without_a_spec() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(
        routes.join("users.rs"),
        "#[vespera::route(get)] pub async fn a() {}\n#[vespera::route(get, path = \"/\")] pub async fn b() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let err = process_vespera_macro(
        &spec_less("routes"),
        &HashMap::new(),
        &[],
        Span::call_site(),
    )
    .expect_err("duplicate route");
    insta::assert_snapshot!("duplicate_route_without_spec", err.to_string());
}

/// Crate-root handlers are named without a module prefix.
#[test]
fn duplicate_route_names_crate_root_handlers() {
    let metadata: crate::metadata::CollectedMetadata = serde_json::from_value(serde_json::json!({
        "routes": [
            {"method": "get", "path": "/", "function_name": "a", "module_path": "", "file_path": "src/lib.rs"},
            {"method": "GET", "path": "/", "function_name": "b", "module_path": "", "file_path": "src/lib.rs"}
        ],
        "structs": []
    }))
    .unwrap();
    let err = crate::openapi_generator::check_duplicate_routes(&metadata).unwrap_err();
    assert!(err.contains("both `a` and `b`"), "{err}");
}

/// A merged app whose exported spec was found is pinned by a compile-time
/// key assertion, and the export publishes that key.
#[test]
fn merged_and_exported_apps_carry_the_export_key() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "parent");
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(
        routes.join("items.rs"),
        "#[vespera::route(get)] pub async fn list() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let exported = process_export_app(
        &syn::parse_quote!(ItemsApp),
        "routes",
        &HashMap::new(),
        &temp_dir.path().to_string_lossy(),
        &[],
        "/items",
        Span::call_site(),
    )
    .unwrap()
    .to_string();
    assert!(exported.contains("__VESPERA_EXPORT_KEY"), "{exported}");
    // The `include_str!` target is content-addressed.
    assert!(exported.contains("ItemsApp.openapi-"), "{exported}");

    let input = ProcessedVesperaInput {
        merge: vec![syn::parse_quote!(crate::ItemsApp)],
        ..spec_less("routes")
    };
    let parent = process_vespera_macro(&input, &HashMap::new(), &[], Span::call_site())
        .unwrap()
        .to_string();
    assert!(
        parent.contains("ItemsApp :: __VESPERA_EXPORT_KEY =="),
        "{parent}"
    );
}

/// The same check guards `export_app!`.
#[test]
fn duplicate_route_is_rejected_by_export_app() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(
        routes.join("items.rs"),
        "#[vespera::route(post)] pub async fn a() {}\n#[vespera::route(post)] pub async fn b() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let err = process_export_app(
        &syn::parse_quote!(ItemsApp),
        "routes",
        &HashMap::new(),
        &temp_dir.path().to_string_lossy(),
        &[],
        "",
        Span::call_site(),
    )
    .expect_err("duplicate route");
    assert!(
        err.to_string()
            .starts_with("export_app! macro: duplicate route: `POST /items`"),
        "{err}"
    );
}
