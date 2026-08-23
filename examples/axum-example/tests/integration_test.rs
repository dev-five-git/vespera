use axum_example::{create_app, create_app_with_layer};
use axum_test::TestServer;
use serde_json::json;
use vespera::axum;

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/health").await;

    response.assert_status_ok();
    response.assert_text("ok");
}

#[tokio::test]
async fn test_mod_file_endpoint() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/hello").await;

    response.assert_status_ok();
    response.assert_text("mod file endpoint");

    let response = server.get("/").await;

    response.assert_status_ok();
    response.assert_text("root endpoint");
}

#[tokio::test]
async fn test_get_users() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/users").await;

    response.assert_status_ok();
    let users: serde_json::Value = response.json();

    assert!(users.is_array());
    assert_eq!(users.as_array().unwrap().len(), 2);

    let first_user = &users[0];
    assert_eq!(first_user["id"], 1);
    assert_eq!(first_user["name"], "Alice");
    assert_eq!(first_user["email"], "alice@example.com");
}

#[tokio::test]
async fn test_get_user_by_id() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/users/42").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();

    assert_eq!(user["id"], 42);
    assert_eq!(user["name"], "User 42");
    assert_eq!(user["email"], "user42@example.com");
}

#[tokio::test]
async fn test_create_user() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let new_user = json!({
        "name": "Charlie",
        "email": "charlie@example.com"
    });

    let response = server.post("/users").json(&new_user).await;

    response.assert_status_ok();
    let created_user: serde_json::Value = response.json();

    assert_eq!(created_user["id"], 100);
    assert_eq!(created_user["name"], "Charlie");
    assert_eq!(created_user["email"], "charlie@example.com");
}

#[tokio::test]
async fn test_get_nonexistent_user() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/users/999").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();
    assert_eq!(user["id"], 999);
}

#[tokio::test]
async fn test_prefix_variable() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/path/prefix/123").await;

    response.assert_status_ok();
    response.assert_text("prefix variable: 123");
}

#[tokio::test]
async fn test_invalid_path() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/nonexistent").await;

    response.assert_status_not_found();
}

#[tokio::test]
async fn test_mod_file_with_complex_struct_body() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let complex_body = json!({
        "name": "Test User",
        "age": 30,
        "nested_struct": {
            "name": "Nested Name",
            "age": 25
        },
        "array": ["item1", "item2", "item3"],
        "map": {
            "key1": "value1",
            "key2": "value2"
        },
        "nested_array": [
            {
                "name": "Array Item 1",
                "age": 20
            },
            {
                "name": "Array Item 2",
                "age": 21
            }
        ],
        "nested_map": {
            "map_key1": {
                "name": "Map Value 1",
                "age": 22
            },
            "map_key2": {
                "name": "Map Value 2",
                "age": 23
            }
        },
        "nested_struct_array": [
            {
                "name": "Struct Array 1",
                "age": 24
            }
        ],
        "nested_struct_map": {
            "struct_map_key": {
                "name": "Struct Map Value",
                "age": 26
            }
        },
        "nested_struct_array_map": [
            {
                "array_map_key1": {
                    "name": "Array Map Value 1",
                    "age": 27
                },
                "array_map_key2": {
                    "name": "Array Map Value 2",
                    "age": 28
                }
            }
        ],
        "nested_struct_map_array": {
            "map_array_key": [
                {
                    "name": "Map Array Value 1",
                    "age": 29
                },
                {
                    "name": "Map Array Value 2",
                    "age": null
                }
            ]
        }
    });

    let response = server
        .post("/complex-struct-body")
        .json(&complex_body)
        .await;

    response.assert_status_ok();
    let response_text = response.text();

    assert!(response_text.contains("name: Test User"));
    assert!(response_text.contains("age: 30"));
    assert!(response_text.contains("item1"));
    assert!(response_text.contains("value1"));
}

#[tokio::test]
async fn test_mod_file_with_complex_struct_body_with_rename() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let complex_body = json!({
        "name": "Test User Renamed",
        "age": 35,
        "nestedStruct": {
            "name": "Nested Name Renamed",
            "age": 30
        },
        "array": ["renamed1", "renamed2", "renamed3"],
        "map": {
            "key1": "renamed_value1",
            "key2": "renamed_value2"
        },
        "nestedArray": [
            {
                "name": "Renamed Array Item 1",
                "age": 25
            },
            {
                "name": "Renamed Array Item 2",
                "age": 26
            }
        ],
        "nestedMap": {
            "map_key1": {
                "name": "Renamed Map Value 1",
                "age": 27
            },
            "map_key2": {
                "name": "Renamed Map Value 2",
                "age": 28
            }
        },
        "nestedStructArray": [
            {
                "name": "Renamed Struct Array 1",
                "age": 29
            }
        ],
        "nestedStructMap": {
            "struct_map_key": {
                "name": "Renamed Struct Map Value",
                "age": 31
            }
        },
        "nestedStructArrayMap": [
            {
                "array_map_key1": {
                    "name": "Renamed Array Map Value 1",
                    "age": 32
                },
                "array_map_key2": {
                    "name": "Renamed Array Map Value 2",
                    "age": 33
                }
            }
        ],
        "nestedStructMapArray": {
            "map_array_key": [
                {
                    "name": "Renamed Map Array Value 1",
                    "age": 34
                },
                {
                    "name": "Renamed Map Array Value 2",
                    "age": null
                }
            ]
        }
    });

    let response = server
        .post("/complex-struct-body-with-rename")
        .json(&complex_body)
        .await;

    response.assert_status_ok();
    let response_text = response.text();

    assert!(response_text.contains("name: Test User Renamed"));
    assert!(response_text.contains("age: 35"));
    assert!(response_text.contains("renamed1"));
    assert!(response_text.contains("renamed_value1"));
}

// Tests for merged routes from third app
#[tokio::test]
async fn test_third_app_root_endpoint() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/third").await;

    response.assert_status_ok();
    response.assert_text("third app root endpoint");
}

#[tokio::test]
async fn test_third_app_hello_endpoint() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/third/hello").await;

    response.assert_status_ok();
    response.assert_text("third app hello endpoint");
}

#[tokio::test]
async fn test_third_app_map_query_endpoint() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/third/map-query?name=test&age=25").await;

    response.assert_status_ok();
    response.assert_text("third app map query endpoint");
}

#[tokio::test]
async fn test_third_app_map_query_with_optional() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server
        .get("/third/map-query?name=test&age=25&optional_age=30")
        .await;

    response.assert_status_ok();
    response.assert_text("third app map query endpoint");
}

#[tokio::test]
async fn test_openapi_contains_third_app_routes() {
    let openapi_content = std::fs::read_to_string("openapi.json").unwrap();
    let openapi: serde_json::Value = serde_json::from_str(&openapi_content).unwrap();

    let paths = openapi.get("paths").unwrap();

    // Verify third app routes are included in the merged OpenAPI spec
    assert!(
        paths.get("/third").is_some(),
        "Missing /third route in OpenAPI spec"
    );
    assert!(
        paths.get("/third/hello").is_some(),
        "Missing /third/hello route in OpenAPI spec"
    );
    assert!(
        paths.get("/third/map-query").is_some(),
        "Missing /third/map-query route in OpenAPI spec"
    );
}

#[tokio::test]
async fn test_openapi_contains_third_app_schemas() {
    let openapi_content = std::fs::read_to_string("openapi.json").unwrap();
    let openapi: serde_json::Value = serde_json::from_str(&openapi_content).unwrap();

    let schemas = openapi.get("components").and_then(|c| c.get("schemas"));

    // Verify third app schemas are included
    assert!(
        schemas.is_some(),
        "Missing components/schemas in OpenAPI spec"
    );
    let schemas = schemas.unwrap();
    assert!(
        schemas.get("ThirdMapQuery").is_some(),
        "Missing ThirdMapQuery schema in OpenAPI spec"
    );
}

// Test VesperaRouter::layer functionality
#[tokio::test]
async fn test_app_with_layer() {
    use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, ORIGIN};

    let app = create_app_with_layer().await;
    let server = TestServer::new(app);

    // Base route works AND the CORS layer is applied (sanity).
    let response = server
        .get("/health")
        .add_header(ORIGIN, "https://example.test")
        .await;
    response.assert_status_ok();
    response.assert_text("ok");
    assert_eq!(
        response
            .headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("*"),
        "CORS layer should be applied to base routes"
    );

    // VESPERA-01 regression lock: the layer must ALSO wrap MERGED child
    // routes.  The original bug applied `layer()` only to the base
    // router, so `/third` (merged from `ThirdApp`) still WORKED but had
    // NO CORS header — a status/text-only test would pass even with the
    // bug.  Asserting the CORS response header on the merged route is
    // what actually proves the fix.
    let response = server
        .get("/third")
        .add_header(ORIGIN, "https://example.test")
        .await;
    response.assert_status_ok();
    response.assert_text("third app root endpoint");
    assert_eq!(
        response
            .headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("*"),
        "CORS layer must apply to MERGED routes too (VESPERA-01)"
    );
}

#[tokio::test]
async fn test_openapi() {
    insta::assert_snapshot!("openapi", std::fs::read_to_string("openapi.json").unwrap());
}

#[tokio::test]
async fn test_openapi_contains_typed_form_routes() {
    let openapi_content = std::fs::read_to_string("openapi.json").unwrap();
    let openapi: serde_json::Value = serde_json::from_str(&openapi_content).unwrap();

    let paths = openapi.get("paths").unwrap();

    // Verify typed-form routes exist
    assert!(
        paths.get("/typed-form").is_some(),
        "Missing /typed-form route in OpenAPI spec"
    );
    assert!(
        paths.get("/typed-form/{id}").is_some(),
        "Missing /typed-form/{{id}} route in OpenAPI spec"
    );

    // Verify POST /typed-form uses multipart/form-data content type
    let post_op = &paths["/typed-form"]["post"];
    let request_body = post_op.get("requestBody").unwrap();
    let content = request_body.get("content").unwrap();
    assert!(
        content.get("multipart/form-data").is_some(),
        "POST /typed-form should use multipart/form-data content type"
    );

    // Verify PUT /typed-form/{id} uses multipart/form-data
    let put_op = &paths["/typed-form/{id}"]["put"];
    let request_body = put_op.get("requestBody").unwrap();
    let content = request_body.get("content").unwrap();
    assert!(
        content.get("multipart/form-data").is_some(),
        "PUT /typed-form/{{id}} should use multipart/form-data content type"
    );

    // Verify PATCH /typed-form/{id} uses multipart/form-data
    let patch_op = &paths["/typed-form/{id}"]["patch"];
    let request_body = patch_op.get("requestBody").unwrap();
    let content = request_body.get("content").unwrap();
    assert!(
        content.get("multipart/form-data").is_some(),
        "PATCH /typed-form/{{id}} should use multipart/form-data content type"
    );
}

#[tokio::test]
async fn test_openapi_memo_detail_same_file_relation_adapter_schema() {
    let openapi_content = std::fs::read_to_string("openapi.json").unwrap();
    let openapi: serde_json::Value = serde_json::from_str(&openapi_content).unwrap();

    let paths = openapi.get("paths").unwrap();
    let schemas = openapi
        .get("components")
        .and_then(|c| c.get("schemas"))
        .unwrap();

    assert!(
        paths.get("/memos/{id}/detail").is_some(),
        "Missing /memos/{{id}}/detail route in OpenAPI spec"
    );

    let memo_detail = &schemas["MemoDetailResponse"];
    // B6: the same-file relation adapter exposes its OWN schema, so the spec
    // matches what the handler actually serializes (UserInMemoDetail's 3 fields)
    // instead of over-promising the base UserSchema's 5 fields.
    // Nullable single-value relation (`BelongsTo` → `Option<..>`) renders as the
    // OpenAPI 3.1 `anyOf: [{$ref}, {type: null}]` form (not the 3.0 `$ref +
    // nullable` keyword), so the adapter $ref lives under `anyOf[0]`.
    assert_eq!(
        memo_detail["properties"]["user"]["anyOf"][0]["$ref"],
        "#/components/schemas/UserInMemoDetail"
    );
    // The referenced adapter schema must carry exactly the adapter's fields —
    // not the base model's createdAt/updatedAt, which never reach the wire.
    let user_props = schemas["UserInMemoDetail"]["properties"]
        .as_object()
        .expect("UserInMemoDetail schema present");
    assert!(user_props.contains_key("id"));
    assert!(user_props.contains_key("email"));
    assert!(user_props.contains_key("name"));
    assert!(
        !user_props.contains_key("createdAt") && !user_props.contains_key("updatedAt"),
        "adapter schema must not over-promise base-model timestamp fields"
    );
    assert_eq!(
        memo_detail["properties"]["memoComments"]["items"]["$ref"],
        "#/components/schemas/MemoCommentInMemoDetail"
    );
    assert!(
        schemas
            .get("__VesperaMemoDetailResponseUserRelation")
            .is_none(),
        "Internal relation adapter should not appear in OpenAPI components"
    );
}

#[tokio::test]
async fn test_openapi_contains_typed_form_schemas() {
    let openapi_content = std::fs::read_to_string("openapi.json").unwrap();
    let openapi: serde_json::Value = serde_json::from_str(&openapi_content).unwrap();

    let schemas = openapi
        .get("components")
        .and_then(|c| c.get("schemas"))
        .unwrap();

    // Verify TypedMultipart request/response schemas exist
    assert!(
        schemas.get("CreateFileUploadRequest").is_some(),
        "Missing CreateFileUploadRequest schema"
    );
    assert!(
        schemas.get("UpdateFileUploadRequest").is_some(),
        "Missing UpdateFileUploadRequest schema"
    );
    assert!(
        schemas.get("PatchFileUploadRequest").is_some(),
        "Missing PatchFileUploadRequest schema (generated via schema_type! multipart)"
    );
    assert!(
        schemas.get("FileUploadResponse").is_some(),
        "Missing FileUploadResponse schema"
    );
}

/// Recursively collect every `$ref` string value in a JSON document.
fn collect_schema_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref" {
                    if let Some(reference) = child.as_str() {
                        out.push(reference.to_string());
                    }
                } else {
                    collect_schema_refs(child, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_schema_refs(item, out);
            }
        }
        _ => {}
    }
}

/// Structural-integrity guard for the generated spec — a regression net for the
/// "wrong data" hunt. Asserts: (1) no dangling component `$ref`, (2) unique
/// `operationId`s, (3) every operation carries a non-empty `responses` object.
/// Locks these invariants so future macro changes cannot silently corrupt the
/// spec the way the original audit findings did.
#[test]
fn test_openapi_structural_integrity() {
    use std::collections::{HashMap, HashSet};

    let openapi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string("openapi.json").unwrap()).unwrap();

    let schema_names: HashSet<&str> = openapi["components"]["schemas"]
        .as_object()
        .expect("components.schemas object")
        .keys()
        .map(String::as_str)
        .collect();

    // 1. No dangling component `$ref`.
    let mut refs = Vec::new();
    collect_schema_refs(&openapi, &mut refs);
    for reference in &refs {
        if let Some(name) = reference.strip_prefix("#/components/schemas/") {
            assert!(
                schema_names.contains(name),
                "dangling $ref to undefined schema: {reference}"
            );
        }
    }

    // 2. Unique operationIds + 3. every operation has a non-empty `responses`.
    const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
    let mut operation_ids: HashMap<String, String> = HashMap::new();
    for (path, item) in openapi["paths"].as_object().expect("paths object") {
        let item = item.as_object().expect("path item object");
        for method in METHODS {
            let Some(op) = item.get(method) else {
                continue;
            };
            let here = format!("{} {path}", method.to_uppercase());

            let responses = op.get("responses").and_then(serde_json::Value::as_object);
            assert!(
                responses.is_some_and(|r| !r.is_empty()),
                "operation {here} has no responses"
            );

            if let Some(op_id) = op.get("operationId").and_then(serde_json::Value::as_str)
                && let Some(prev) = operation_ids.insert(op_id.to_string(), here.clone())
            {
                panic!("duplicate operationId '{op_id}': {prev} and {here}");
            }
        }
    }
}
