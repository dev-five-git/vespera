use std::{collections::HashMap, fs, path::PathBuf};

use rstest::rstest;
use tempfile::TempDir;

use crate::{
    metadata::{CollectedMetadata, RouteMetadata, StructMetadata},
    openapi_generator::generate_openapi_doc_with_metadata,
    route_impl::StoredRouteInfo,
};

fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
    let file_path = dir.path().join(filename);
    fs::write(&file_path, content).expect("Failed to write temp file");
    file_path
}

/// Build a `RouteMetadata` with the boilerplate-heavy fields defaulted.
fn route_meta(method: &str, path: &str, fn_name: &str, file_path: &str) -> RouteMetadata {
    RouteMetadata {
        method: method.to_string(),
        path: path.to_string(),
        function_name: fn_name.to_string(),
        module_path: format!("test::{fn_name}"),
        file_path: file_path.to_string(),
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        success_status: None,
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
    }
}

#[test]
fn route_in_file_cache_appears_in_paths() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> String { \"users\".to_string() }",
    );
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route_meta(
        "GET",
        "/users",
        "get_users",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    let op = doc
        .paths
        .get("/users")
        .and_then(|p| p.get.as_ref())
        .expect("GET op");
    assert_eq!(op.operation_id.as_deref(), Some("get_users"));
}

#[test]
fn duplicate_method_and_path_is_a_compile_error() {
    // Two distinct handlers mapping to the same (GET, /dup) must be a compile
    // error that names BOTH handlers — not a silent last-wins overwrite that
    // drops a route from the generated spec (axum panics on this at runtime).
    let route_file_path = "/virtual/dup.rs".to_string();
    let route_src = "pub fn first() -> String { String::new() }\n\
                     pub fn second() -> String { String::new() }";
    let parsed: syn::File = syn::parse_str(route_src).expect("route src parses");
    let mut file_cache: HashMap<String, syn::File> = HashMap::new();
    file_cache.insert(route_file_path.clone(), parsed);

    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/dup", "first", &route_file_path));
    metadata
        .routes
        .push(route_meta("GET", "/dup", "second", &route_file_path));

    let err = super::build_path_items(
        &metadata,
        &std::collections::HashSet::new(),
        &HashMap::new(),
        &file_cache,
        &[],
    )
    .expect_err("duplicate (GET, /dup) must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("duplicate route"), "unexpected message: {msg}");
    assert!(
        msg.contains("first") && msg.contains("second"),
        "message should name both handlers: {msg}"
    );
}

#[test]
fn route_storage_dedup_skips_already_in_ast() {
    // When a route's `fn_sig_str` was already discovered by parsing the
    // source file via `file_cache`, the storage-parse step must skip
    // re-parsing it — exercises the `already_in_ast → return None`
    // branch inside `route_fn_cache` construction.
    let route_file_path = "/virtual/users.rs".to_string();
    let route_src = "pub fn get_users() -> String { \"users\".to_string() }";
    let parsed: syn::File = syn::parse_str(route_src).expect("route src parses");
    let mut file_cache: HashMap<String, syn::File> = HashMap::new();
    file_cache.insert(route_file_path.clone(), parsed);

    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/users", "get_users", &route_file_path));

    let route_storage = vec![StoredRouteInfo {
        fn_name: "get_users".to_string(),
        method: Some("get".to_string()),
        custom_path: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        success_status: None,
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        file_path: Some(route_file_path),
        fn_sig_str: route_src.to_string(),
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

    let op = doc
        .paths
        .get("/users")
        .and_then(|p| p.get.as_ref())
        .expect("GET op");
    assert_eq!(op.operation_id.as_deref(), Some("get_users"));
}

#[test]
fn route_storage_fast_path_when_fn_not_in_file_cache() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> String { \"users\".to_string() }\n",
    );
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route_meta(
        "GET",
        "/users",
        "get_users",
        &route_file.to_string_lossy(),
    ));
    let route_storage = vec![StoredRouteInfo {
        fn_name: "get_users".to_string(),
        method: Some("get".to_string()),
        custom_path: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        success_status: None,
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        fn_sig_str: "fn get_users() -> String".to_string(),
        file_path: None,
    }];

    let doc =
        generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &route_storage);

    let op = doc
        .paths
        .get("/users")
        .and_then(|p| p.get.as_ref())
        .expect("GET op");
    assert_eq!(op.operation_id.as_deref(), Some("get_users"));
}

#[test]
fn route_storage_fast_path_disambiguates_same_fn_name_by_file_path() {
    let users_path = "/virtual/users.rs".to_string();
    let posts_path = "/virtual/posts.rs".to_string();
    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/users", "list", &users_path));
    metadata
        .routes
        .push(route_meta("GET", "/posts", "list", &posts_path));

    let route_storage = vec![
        StoredRouteInfo {
            fn_name: "list".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: "fn list() -> String".to_string(),
            file_path: Some(users_path),
        },
        StoredRouteInfo {
            fn_name: "list".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: "fn list() -> i32".to_string(),
            file_path: Some(posts_path),
        },
    ];

    let doc =
        generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &route_storage);

    let users_schema = doc
        .paths
        .get("/users")
        .and_then(|path| path.get.as_ref())
        .and_then(|op| op.responses.get("200"))
        .and_then(|response| response.content.as_ref())
        .and_then(|content| content.values().next())
        .and_then(|media| media.schema.as_ref())
        .expect("users response schema");
    let posts_schema = doc
        .paths
        .get("/posts")
        .and_then(|path| path.get.as_ref())
        .and_then(|op| op.responses.get("200"))
        .and_then(|response| response.content.as_ref())
        .and_then(|content| content.values().next())
        .and_then(|media| media.schema.as_ref())
        .expect("posts response schema");

    let schema_type = |schema: &vespera_core::schema::SchemaRef| match schema {
        vespera_core::schema::SchemaRef::Inline(schema) => schema.schema_type,
        vespera_core::schema::SchemaRef::Ref(reference) => {
            panic!("expected inline schema, got {}", reference.ref_path)
        }
    };
    assert_eq!(
        schema_type(users_schema),
        Some(vespera_core::schema::SchemaType::String)
    );
    assert_eq!(
        schema_type(posts_schema),
        Some(vespera_core::schema::SchemaType::Integer)
    );
}

#[test]
fn route_storage_legacy_none_file_path_is_skipped_when_ambiguous() {
    let users_path = "/virtual/users.rs".to_string();
    let posts_path = "/virtual/posts.rs".to_string();
    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/users", "list", &users_path));
    metadata
        .routes
        .push(route_meta("GET", "/posts", "list", &posts_path));

    let mut file_cache = HashMap::new();
    file_cache.insert(
        users_path.clone(),
        syn::parse_str("pub fn list() -> String { String::new() }").unwrap(),
    );
    file_cache.insert(
        posts_path.clone(),
        syn::parse_str("pub fn list() -> i32 { 1 }").unwrap(),
    );

    let route_storage = vec![
        StoredRouteInfo {
            fn_name: "list".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: "fn list() -> bool".to_string(),
            file_path: None,
        },
        StoredRouteInfo {
            fn_name: "list".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: "fn list() -> bool".to_string(),
            file_path: None,
        },
    ];

    let doc = generate_openapi_doc_with_metadata(
        None,
        None,
        None,
        None,
        &metadata,
        Some(file_cache),
        &route_storage,
    );

    let response_schema_type = |path: &str| {
        let schema = doc
            .paths
            .get(path)
            .and_then(|path| path.get.as_ref())
            .and_then(|op| op.responses.get("200"))
            .and_then(|response| response.content.as_ref())
            .and_then(|content| content.values().next())
            .and_then(|media| media.schema.as_ref())
            .expect("response schema");
        match schema {
            vespera_core::schema::SchemaRef::Inline(schema) => schema.schema_type,
            vespera_core::schema::SchemaRef::Ref(reference) => {
                panic!("expected inline schema, got {}", reference.ref_path)
            }
        }
    };

    assert_eq!(
        response_schema_type("/users"),
        Some(vespera_core::schema::SchemaType::String)
    );
    assert_eq!(
        response_schema_type("/posts"),
        Some(vespera_core::schema::SchemaType::Integer)
    );
}

#[test]
fn route_with_function_not_in_ast_is_skipped() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_items() -> String { \"items\".to_string() }\n",
    );
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route_meta(
        "GET",
        "/users",
        "get_users",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert!(
        doc.paths.is_empty(),
        "Route with non-matching function should be skipped"
    );
}

#[test]
fn route_and_struct_appear_together() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "user_route.rs",
        r#"
use crate::user::User;

pub fn get_user() -> User {
User { id: 1, name: "Alice".to_string() }
}
"#,
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(StructMetadata {
        name: "User".to_string(),
        definition: "struct User { id: i32, name: String }".to_string(),
        ..Default::default()
    });
    metadata.routes.push(route_meta(
        "GET",
        "/user",
        "get_user",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(
        Some("Test API".to_string()),
        Some("1.0.0".to_string()),
        None,
        None,
        &metadata,
        None,
        &[],
    );

    let schemas = doc
        .components
        .as_ref()
        .and_then(|c| c.schemas.as_ref())
        .expect("schemas present");
    assert!(schemas.contains_key("User"));
    assert!(
        doc.paths
            .get("/user")
            .and_then(|p| p.get.as_ref())
            .is_some()
    );
}

#[test]
fn multiple_methods_share_path_item() {
    let temp_dir = TempDir::new().unwrap();
    let r1 = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> String { \"users\".to_string() }",
    );
    let r2 = create_temp_file(
        &temp_dir,
        "create_user.rs",
        "pub fn create_user() -> String { \"created\".to_string() }",
    );

    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route_meta(
        "GET",
        "/users",
        "get_users",
        &r1.to_string_lossy(),
    ));
    metadata.routes.push(route_meta(
        "POST",
        "/users",
        "create_user",
        &r2.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert_eq!(doc.paths.len(), 1);
    let path_item = doc.paths.get("/users").unwrap();
    assert!(path_item.get.is_some());
    assert!(path_item.post.is_some());
}

#[test]
fn tags_and_description_propagate_to_operation() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> String { \"users\".to_string() }",
    );

    let mut metadata = CollectedMetadata::new();
    let mut rm = route_meta("GET", "/users", "get_users", &route_file.to_string_lossy());
    rm.error_status = Some(vec![404]);
    rm.tags = Some(vec!["users".to_string(), "admin".to_string()]);
    rm.description = Some("Get all users".to_string());
    metadata.routes.push(rm);

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    let op = doc
        .paths
        .get("/users")
        .and_then(|p| p.get.as_ref())
        .unwrap();
    assert_eq!(op.description.as_deref(), Some("Get all users"));
    let tags = doc.tags.as_ref().expect("tags present");
    assert!(tags.iter().any(|t| t.name == "users"));
    assert!(tags.iter().any(|t| t.name == "admin"));
}

/// File-read / parse failures must not produce phantom routes or schemas.
#[rstest]
#[case::route_file_read_failure("/nonexistent/route.rs", None)]
#[case::route_file_parse_failure("", Some("invalid rust syntax {"))]
fn file_errors_skip_route(#[case] file_path_template: &str, #[case] write_invalid: Option<&str>) {
    let temp_dir = TempDir::new().unwrap();
    let final_file_path = write_invalid.map_or_else(
        || file_path_template.to_string(),
        |content| {
            create_temp_file(&temp_dir, "invalid_route.rs", content)
                .to_string_lossy()
                .to_string()
        },
    );

    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("GET", "/users", "get_users", &final_file_path));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert!(!doc.paths.contains_key("/users"));
    // schemas must also be empty — no struct was registered.
    if let Some(schemas) = doc.components.as_ref().and_then(|c| c.schemas.as_ref()) {
        assert!(!schemas.contains_key("User"));
    }
}

#[test]
fn unknown_http_method_route_is_compile_error() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> String { \"users\".to_string() }",
    );

    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route_meta(
        "INVALID",
        "/users",
        "get_users",
        &route_file.to_string_lossy(),
    ));

    let err = crate::openapi_generator::try_generate_openapi_doc_with_metadata(
        None,
        None,
        None,
        None,
        &metadata,
        None,
        &[],
    )
    .expect_err("unknown method should fail OpenAPI generation");

    assert!(err.to_string().contains("unsupported HTTP method"));
}

#[test]
fn unknown_method_fails_even_when_valid_route_exists() {
    let temp_dir = TempDir::new().unwrap();
    let route_file = create_temp_file(
        &temp_dir,
        "users.rs",
        r#"
pub fn get_users() -> String
{ "users".to_string() }

pub fn create_users() -> String { "created".to_string() }
"#,
    );
    let file_path = route_file.to_string_lossy().to_string();

    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route_meta("CONNECT", "/users", "get_users", &file_path));
    metadata
        .routes
        .push(route_meta("POST", "/users", "create_users", &file_path));

    let err = crate::openapi_generator::try_generate_openapi_doc_with_metadata(
        None,
        None,
        None,
        None,
        &metadata,
        None,
        &[],
    )
    .expect_err("unknown method should fail OpenAPI generation");

    assert!(err.to_string().contains("unsupported HTTP method"));
}
