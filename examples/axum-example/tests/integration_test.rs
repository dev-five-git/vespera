use axum_example::{create_app, create_app_with_layer};
use axum_test::TestServer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vespera::{Schema, schema};

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/health").await;

    response.assert_status_ok();
    response.assert_text("ok");
}

#[tokio::test]
async fn test_mod_file_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/hello").await;

    response.assert_status_ok();
    response.assert_text("mod file endpoint");

    let response = server.get("/").await;

    response.assert_status_ok();
    response.assert_text("root endpoint");
}

#[tokio::test]
async fn test_get_users() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

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
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users/42").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();

    assert_eq!(user["id"], 42);
    assert_eq!(user["name"], "User 42");
    assert_eq!(user["email"], "user42@example.com");
}

#[tokio::test]
async fn test_create_user() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

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
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users/999").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();
    assert_eq!(user["id"], 999);
}

#[tokio::test]
async fn test_prefix_variable() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/path/prefix/123").await;

    response.assert_status_ok();
    response.assert_text("prefix variable: 123");
}

#[tokio::test]
async fn test_invalid_path() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/nonexistent").await;

    response.assert_status_not_found();
}

#[tokio::test]
async fn test_mod_file_with_complex_struct_body() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

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
    let app = create_app();
    let server = TestServer::new(app).unwrap();

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
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/third").await;

    response.assert_status_ok();
    response.assert_text("third app root endpoint");
}

#[tokio::test]
async fn test_third_app_hello_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/third/hello").await;

    response.assert_status_ok();
    response.assert_text("third app hello endpoint");
}

#[tokio::test]
async fn test_third_app_map_query_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/third/map-query?name=test&age=25").await;

    response.assert_status_ok();
    response.assert_text("third app map query endpoint");
}

#[tokio::test]
async fn test_third_app_map_query_with_optional() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

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
    let app = create_app_with_layer();
    let server = TestServer::new(app).unwrap();

    // Test that routes still work with the layer applied
    let response = server.get("/health").await;
    response.assert_status_ok();
    response.assert_text("ok");

    // Test merged routes also work with layer
    let response = server.get("/third").await;
    response.assert_status_ok();
    response.assert_text("third app root endpoint");
}

#[tokio::test]
async fn test_openapi() {
    insta::assert_snapshot!("openapi", std::fs::read_to_string("openapi.json").unwrap());
}

// Tests for schema! macro
// Note: schema! requires #[derive(Schema)] in the same compilation unit,
// so we define the test structs here.

/// Test struct for schema! macro tests
#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct TestUser {
    pub id: u32,
    pub name: String,
    pub email: String,
}

/// Test struct with optional fields
#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct TestUserWithOptional {
    pub id: u32,
    pub name: String,
    pub email: Option<String>,
    #[serde(default)]
    pub bio: String,
}

/// Test struct with serde rename
#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(rename_all = "camelCase")]
pub struct TestUserCamelCase {
    pub user_id: u32,
    pub user_name: String,
    pub email_address: String,
}

#[test]
fn test_schema_macro_full() {
    // Generate full schema for TestUser
    let user_schema = schema!(TestUser);

    // Verify schema type
    assert_eq!(
        user_schema.schema_type,
        Some(vespera::schema::SchemaType::Object)
    );

    // Verify all properties are present
    let properties = user_schema.properties.unwrap();
    assert!(properties.contains_key("id"), "Missing 'id' property");
    assert!(properties.contains_key("name"), "Missing 'name' property");
    assert!(properties.contains_key("email"), "Missing 'email' property");

    // Verify required fields
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
    assert!(required.contains(&"email".to_string()));
}

#[test]
fn test_schema_macro_with_omit() {
    // Generate schema with 'email' field omitted
    let user_schema = schema!(TestUser, omit = ["email"]);

    // Verify schema type
    assert_eq!(
        user_schema.schema_type,
        Some(vespera::schema::SchemaType::Object)
    );

    // Verify properties - email should be omitted
    let properties = user_schema.properties.unwrap();
    assert!(properties.contains_key("id"), "Missing 'id' property");
    assert!(properties.contains_key("name"), "Missing 'name' property");
    assert!(
        !properties.contains_key("email"),
        "'email' should be omitted"
    );

    // Verify required fields - email should not be in required
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
    assert!(!required.contains(&"email".to_string()));
}

#[test]
fn test_schema_macro_with_multiple_omit() {
    // Generate schema with multiple fields omitted
    let user_schema = schema!(TestUser, omit = ["id", "email"]);

    // Verify properties - id and email should be omitted
    let properties = user_schema.properties.unwrap();
    assert!(!properties.contains_key("id"), "'id' should be omitted");
    assert!(properties.contains_key("name"), "Missing 'name' property");
    assert!(
        !properties.contains_key("email"),
        "'email' should be omitted"
    );

    // Verify only 'name' is required
    let required = user_schema.required.unwrap();
    assert_eq!(required.len(), 1);
    assert!(required.contains(&"name".to_string()));
}

#[test]
fn test_schema_macro_with_pick() {
    // Generate schema with only 'id' and 'name' fields
    let user_schema = schema!(TestUser, pick = ["id", "name"]);

    // Verify properties - only id and name should be present
    let properties = user_schema.properties.unwrap();
    assert!(properties.contains_key("id"), "Missing 'id' property");
    assert!(properties.contains_key("name"), "Missing 'name' property");
    assert!(
        !properties.contains_key("email"),
        "'email' should not be picked"
    );

    // Verify required fields
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
}

#[test]
fn test_schema_macro_with_optional_fields() {
    // Generate schema for struct with optional fields
    let user_schema = schema!(TestUserWithOptional);

    let properties = user_schema.properties.unwrap();
    assert_eq!(properties.len(), 4);

    // Only 'id' and 'name' should be required
    // 'email' is Option<T> and 'bio' has #[serde(default)]
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
    assert!(
        !required.contains(&"email".to_string()),
        "'email' is Option<T>, should not be required"
    );
    assert!(
        !required.contains(&"bio".to_string()),
        "'bio' has default, should not be required"
    );
}

#[test]
fn test_schema_macro_with_rename_all() {
    // Generate schema for struct with rename_all = "camelCase"
    let user_schema = schema!(TestUserCamelCase);

    let properties = user_schema.properties.unwrap();

    // Properties should have camelCase names
    assert!(
        properties.contains_key("userId"),
        "Missing 'userId' property (renamed from user_id)"
    );
    assert!(
        properties.contains_key("userName"),
        "Missing 'userName' property (renamed from user_name)"
    );
    assert!(
        properties.contains_key("emailAddress"),
        "Missing 'emailAddress' property (renamed from email_address)"
    );

    // Should NOT have snake_case names
    assert!(!properties.contains_key("user_id"));
    assert!(!properties.contains_key("user_name"));
    assert!(!properties.contains_key("email_address"));
}

#[test]
fn test_schema_macro_omit_with_renamed_field() {
    // Omit using the JSON name (camelCase)
    let user_schema = schema!(TestUserCamelCase, omit = ["emailAddress"]);

    let properties = user_schema.properties.unwrap();
    assert!(properties.contains_key("userId"));
    assert!(properties.contains_key("userName"));
    assert!(
        !properties.contains_key("emailAddress"),
        "'emailAddress' should be omitted"
    );
}

#[test]
fn test_schema_macro_omit_with_rust_field_name() {
    // Omit using the Rust field name (snake_case) - should also work
    let user_schema = schema!(TestUserCamelCase, omit = ["email_address"]);

    let properties = user_schema.properties.unwrap();
    assert!(properties.contains_key("userId"));
    assert!(properties.contains_key("userName"));
    assert!(
        !properties.contains_key("emailAddress"),
        "'email_address' (rust name) should omit 'emailAddress'"
    );
}

// Tests for schema_type! with rename option

#[tokio::test]
async fn test_get_user_dto_with_renamed_fields() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users/dto/42").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();

    // JSON should use original field names (id, name) due to serde(rename)
    // even though Rust struct uses user_id, display_name
    assert_eq!(user["id"], 42, "JSON should serialize 'user_id' as 'id'");
    assert_eq!(
        user["name"], "User 42",
        "JSON should serialize 'display_name' as 'name'"
    );

    // Verify renamed field names are NOT in JSON
    assert!(
        user.get("user_id").is_none(),
        "'user_id' should not appear in JSON"
    );
    assert!(
        user.get("display_name").is_none(),
        "'display_name' should not appear in JSON"
    );
}

// Tests for schema_type! with add option

#[tokio::test]
async fn test_create_user_with_meta_add_fields() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    // CreateUserWithMeta has: name, email (from User) + request_id, created_at (added)
    let request_body = json!({
        "name": "Test User",
        "email": "test@example.com",
        "request_id": "req-12345",
        "created_at": null
    });

    let response = server.post("/users/with-meta").json(&request_body).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    // Verify fields from User (picked)
    assert_eq!(result["name"], "Test User");
    assert_eq!(result["email"], "test@example.com");

    // Verify added fields
    assert_eq!(result["request_id"], "req-12345");
    assert_eq!(result["created_at"], "2024-01-27T12:00:00Z"); // Server fills this in
}

// Tests for schema_type! with sea-orm-like models

#[tokio::test]
async fn test_memo_create_with_picked_fields() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    // CreateMemoRequest has only: title, content (picked from Memo)
    let request_body = json!({
        "title": "Test Memo",
        "content": "This is test content"
    });

    let response = server.post("/memos").json(&request_body).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["title"], "Test Memo");
    assert_eq!(result["content"], "This is test content");

    // These fields should NOT be in the response (not picked)
    assert!(
        result.get("id").is_none(),
        "id should not be in CreateMemoRequest"
    );
    assert!(
        result.get("created_at").is_none(),
        "created_at should not be in CreateMemoRequest"
    );
}

#[tokio::test]
async fn test_memo_update_with_added_id_field() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    // UpdateMemoRequest has: title, content (picked) + id (added)
    let request_body = json!({
        "id": 42,
        "title": "Updated Memo",
        "content": "Updated content"
    });

    let response = server.put("/memos").json(&request_body).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    // Verify picked fields
    assert_eq!(result["title"], "Updated Memo");
    assert_eq!(result["content"], "Updated content");

    // Verify added field
    assert_eq!(result["id"], 42, "id should be present (added field)");
}
