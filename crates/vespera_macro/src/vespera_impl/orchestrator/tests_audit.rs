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

/// `/{id}` GET next to `/{user_id}` DELETE in one file: axum panics on the
/// differently named parameters of one path shape whatever the methods.
#[test]
fn param_name_only_difference_is_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(
        routes.join("users.rs"),
        "#[vespera::route(get, path = \"/{id}\")] pub async fn get_user() {}\n#[vespera::route(delete, path = \"/{user_id}\")] pub async fn delete_user() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let err = process_vespera_macro(
        &spec_less("routes"),
        &HashMap::new(),
        &[],
        Span::call_site(),
    )
    .expect_err("conflicting parameter names");
    insta::assert_snapshot!("param_name_conflict_without_spec", err.to_string());
}

/// A documentation route shadowing an app route is a compile error.
#[test]
fn doc_route_shadowing_an_app_route_is_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(
        routes.join("docs.rs"),
        "#[vespera::route(get)] pub async fn page() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let input = ProcessedVesperaInput {
        docs_url: Some("/docs".to_string()),
        ..spec_less("routes")
    };
    let err = process_vespera_macro(&input, &HashMap::new(), &[], Span::call_site())
        .expect_err("docs_url shadows GET /docs");
    insta::assert_snapshot!("doc_route_conflict_end_to_end", err.to_string());
}

/// rust-analyzer keeps a renamed handler's old `ROUTE_STORAGE` entry; the
/// router must mount only functions the file still declares.
#[test]
fn stale_storage_entries_of_renamed_handlers_are_ignored() {
    let temp_dir = TempDir::new().unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    let file = routes.join("users.rs");
    fs::write(&file, "#[vespera::route(get)] pub async fn list_v2() {}\n").unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let stored = |name: &str| crate::route_impl::StoredRouteInfo {
        fn_name: name.to_string(),
        method: Some("get".to_string()),
        file_path: Some(file.display().to_string()),
        fn_sig_str: format!("async fn {name}()"),
        inline_module: Some(String::new()),
        ..Default::default()
    };
    let storage = [stored("list"), stored("list_v2")];

    let generated = process_vespera_macro(
        &spec_less("routes"),
        &HashMap::new(),
        &storage,
        Span::call_site(),
    )
    .unwrap()
    .to_string();
    assert!(generated.contains("list_v2"), "{generated}");
    assert!(!generated.contains("users :: list)"), "{generated}");
    let exported = process_export_app(
        &syn::parse_quote!(UsersApp),
        "routes",
        &HashMap::new(),
        &temp_dir.path().to_string_lossy(),
        &storage,
        "",
        Span::call_site(),
    )
    .unwrap()
    .to_string();
    assert!(!exported.contains("users :: list)"), "{exported}");
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
