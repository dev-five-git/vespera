//! ROUTE_STORAGE population tests: the records `process_route_attribute` stores
//! for `vespera!` (kept out of `route_impl.rs` for the 1000-line cap).

use quote::quote;

use super::*;

// ========== Tests for ROUTE_STORAGE population ==========

#[test]
fn test_route_storage_populated_by_process_route_attribute() {
    let attr = quote!(
        get,
        path = "/{id}",
        tags = ["users"],
        description = "Get user by ID",
        error_status = [404]
    );
    let item = quote!(
        pub async fn get_user_test_storage() -> String {
            "test".to_string()
        }
    );
    let result = process_route_attribute(attr, item);
    assert!(result.is_ok());

    // Find our entry by unique fn_name (the current crate's routes are
    // shared across parallel tests in this crate).
    let storage = current_crate_routes();

    // Find our entry and verify fields
    let stored = storage
        .iter()
        .find(|s| s.fn_name == "get_user_test_storage");
    assert!(
        stored.is_some(),
        "StoredRouteInfo should be in ROUTE_STORAGE"
    );
    let stored = stored.unwrap();
    assert_eq!(stored.method, Some("get".to_string()));
    assert_eq!(stored.custom_path, Some("/{id}".to_string()));
    assert_eq!(stored.tags, Some(vec!["users".to_string()]));
    assert_eq!(stored.description, Some("Get user by ID".to_string()));
    assert_eq!(stored.error_status, Some(vec![404]));
    assert_eq!(stored.headers.len(), 0);
    assert!(stored.fn_sig_str.contains("get_user_test_storage"));
}

#[test]
fn test_route_storage_no_optional_fields() {
    let attr = quote!();
    let item = quote!(
        pub async fn minimal_handler_test() -> String {
            "test".to_string()
        }
    );
    let result = process_route_attribute(attr, item);
    assert!(result.is_ok());

    let storage = current_crate_routes();

    let stored = storage.iter().find(|s| s.fn_name == "minimal_handler_test");
    assert!(stored.is_some());
    let stored = stored.unwrap();
    assert_eq!(stored.method, None);
    assert_eq!(stored.custom_path, None);
    assert_eq!(stored.tags, None);
    assert_eq!(stored.description, None);
    assert_eq!(stored.error_status, None);
    assert_eq!(stored.headers.len(), 0);
}

#[test]
fn test_register_route_replaces_same_file_and_function() {
    let file_path = Some("/tmp/vespera/routes/replaced.rs".to_string());
    let fn_name = "__test_replace_route".to_string();
    let base = StoredRouteInfo {
        own_test_only: false,
        fn_name: fn_name.clone(),
        method: Some("get".to_string()),
        custom_path: Some("/before".to_string()),
        success_status: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        file_path: file_path.clone(),
        fn_sig_str: "pub async fn __test_replace_route ()".to_string(),
        inline_module: None,
    };
    register_route(base);
    register_route(StoredRouteInfo {
        own_test_only: false,
        method: Some("post".to_string()),
        custom_path: Some("/after".to_string()),
        file_path,
        fn_sig_str: "pub async fn __test_replace_route ()".to_string(),
        inline_module: None,
        ..current_crate_routes()
            .iter()
            .find(|entry| entry.fn_name == fn_name)
            .cloned()
            .expect("first route registration should exist")
    });

    let matches: Vec<_> = current_crate_routes()
        .iter()
        .filter(|entry| entry.fn_name == fn_name)
        .cloned()
        .collect();
    assert_eq!(matches.len(), 1, "same source route should replace");
    assert_eq!(matches[0].method, Some("post".to_string()));
    assert_eq!(matches[0].custom_path, Some("/after".to_string()));
}

/// A top-level handler and a same-named one inside an inline module are
/// two entries — the inline one must not overwrite (and so hide) the
/// top-level registration, nor vice versa.
#[test]
fn test_register_route_keys_by_inline_module() {
    let file_path = Some("/tmp/vespera/routes/scoped.rs".to_string());
    let fn_name = "__test_scoped_route".to_string();
    for scope in ["", "v2"] {
        register_route(StoredRouteInfo {
            own_test_only: false,
            fn_name: fn_name.clone(),
            file_path: file_path.clone(),
            inline_module: Some(scope.to_string()),
            ..StoredRouteInfo::default()
        });
    }
    let scopes: Vec<_> = current_crate_routes()
        .iter()
        .filter(|entry| entry.fn_name == fn_name)
        .map(|entry| entry.inline_module.clone())
        .collect();
    assert_eq!(scopes, [Some(String::new()), Some("v2".to_string())]);
}

/// `#[route]` reads files through the file cache (Path-extractor check,
/// `inline_module_of`), so every invocation starts a fresh epoch.
#[test]
fn test_route_attribute_bumps_the_file_cache_epoch() {
    let before = crate::schema_macro::file_cache::current_epoch();
    process_route_attribute(
        quote!(get),
        quote!(
            pub async fn epoch_probe() {}
        ),
    )
    .unwrap();
    assert_ne!(crate::schema_macro::file_cache::current_epoch(), before);
}
