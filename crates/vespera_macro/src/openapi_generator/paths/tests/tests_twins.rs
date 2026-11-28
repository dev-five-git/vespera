//! Route files that declare a handler twice (`#[cfg]` twins).

use super::*;

/// A route file declaring its handler twice (a `#[cfg]` twin) is ambiguous:
/// the file AST cannot say which one the route is, so the route's own stored
/// signature is used and the file's last same-named fn is not.
#[test]
fn cfg_twin_route_handler_falls_back_to_the_stored_signature() {
    let route_file_path = "/virtual/twins.rs".to_string();
    let parsed: syn::File = syn::parse_str(
        "#[cfg(test)] pub fn get_users() -> bool { true }
         pub fn get_users() -> String { \"users\".to_string() }",
    )
    .expect("route src parses");
    let mut file_cache: HashMap<String, syn::File> = HashMap::new();
    file_cache.insert(route_file_path.clone(), parsed);

    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/users", "get_users", &route_file_path));

    let route_storage = vec![StoredRouteInfo {
        own_test_only: false,
        fn_name: "get_users".to_string(),
        method: Some("get".to_string()),
        file_path: Some(route_file_path),
        fn_sig_str: "fn get_users() -> i32".to_string(),
        ..StoredRouteInfo::default()
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

    let schema = doc
        .paths
        .get("/users")
        .and_then(|p| p.get.as_ref())
        .and_then(|op| op.responses.get("200"))
        .and_then(|response| response.content.as_ref())
        .and_then(|content| content.values().next())
        .and_then(|media| media.schema.as_ref())
        .expect("response schema");
    let schema = serde_json::to_value(schema).expect("schema serialises");
    assert_eq!(schema["type"], "integer", "{schema}");
}
