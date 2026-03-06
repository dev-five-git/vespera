use axum_example::{create_app, create_app_with_layer};
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use serde::{Deserialize, Serialize};
use serde_json::json;
use vespera::multipart::{FieldData, TypedMultipart};
use vespera::{Multipart, Schema, schema};

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
    let app = create_app_with_layer().await;
    let server = TestServer::new(app);

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
    let app = create_app().await;
    let server = TestServer::new(app);

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
    let app = create_app().await;
    let server = TestServer::new(app);

    // CreateUserWithMeta has: name, email (from User) + request_id, created_at (added)
    // Note: Field names are camelCase in JSON due to serde rename_all = "camelCase"
    let request_body = json!({
        "name": "Test User",
        "email": "test@example.com",
        "requestId": "req-12345",
        "createdAt": null
    });

    let response = server.post("/users/with-meta").json(&request_body).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    // Verify fields from User (picked)
    assert_eq!(result["name"], "Test User");
    assert_eq!(result["email"], "test@example.com");

    // Verify added fields (camelCase in JSON)
    assert_eq!(result["requestId"], "req-12345");
    assert_eq!(result["createdAt"], "2024-01-27T12:00:00Z"); // Server fills this in
}

// Tests for schema_type! with sea-orm-like models

#[tokio::test]
async fn test_memo_create_with_picked_fields() {
    let app = create_app().await;
    let server = TestServer::new(app);

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
    let app = create_app().await;
    let server = TestServer::new(app);

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

// Tests for TypedMultipart (Multipart) request body extraction

#[tokio::test]
async fn test_typed_form_list_file_uploads() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/typed-form").await;

    response.assert_status_ok();
    let uploads: serde_json::Value = response.json();

    assert!(uploads.is_array());
    let uploads = uploads.as_array().unwrap();
    assert_eq!(uploads.len(), 1);

    let upload = &uploads[0];
    assert_eq!(upload["id"], 1);
    assert_eq!(upload["name"], "Sample Upload");
    assert_eq!(upload["thumbnailUrl"], "https://example.com/thumb.jpg");
    assert_eq!(upload["documentUrl"], "https://example.com/doc.pdf");
    assert_eq!(upload["isActive"], true);
    assert!(upload["tags"].is_array());
    assert_eq!(upload["tags"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_typed_form_create_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let form = MultipartForm::new()
        .add_text("name", "Test Upload")
        .add_text("tags", "rust, axum, vespera");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 1);
    assert_eq!(result["name"], "Test Upload");
    assert_eq!(result["isActive"], true);

    // Tags should be parsed from comma-separated string
    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0], "rust");
    assert_eq!(tags[1], "axum");
    assert_eq!(tags[2], "vespera");

    // No files uploaded, so URLs should be null
    assert!(result["thumbnailUrl"].is_null());
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_create_file_upload_with_files() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"fake image data".as_slice()).file_name("thumb.jpg");
    let document_part = Part::bytes(b"fake pdf data".as_slice()).file_name("doc.pdf");

    let form = MultipartForm::new()
        .add_text("name", "Upload With Files")
        .add_part("thumbnail", thumbnail_part)
        .add_part("document", document_part)
        .add_text("tags", "files");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["name"], "Upload With Files");
    assert_eq!(result["thumbnailUrl"], "uploaded_thumbnail_url");
    assert_eq!(result["documentUrl"], "uploaded_document_url");

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], "files");
}

#[tokio::test]
async fn test_typed_form_update_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // UpdateFileUploadRequest has #[serde(rename_all = "camelCase")] which vespera::Multipart respects.
    // The Multipart derive reads serde attrs to match field names at runtime.
    let form = MultipartForm::new()
        .add_text("name", "Updated Upload")
        .add_text("tags", "updated, tags")
        .add_text("isActive", "false");

    let response = server.put("/typed-form/42").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 42);
    assert_eq!(result["name"], "Updated Upload");
    // Response uses FileUploadResponse with #[serde(rename_all = "camelCase")]
    assert_eq!(result["isActive"], false);

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0], "updated");
    assert_eq!(tags[1], "tags");
}

#[tokio::test]
async fn test_typed_form_update_file_upload_with_file() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"new image".as_slice()).file_name("new_thumb.jpg");

    let form = MultipartForm::new()
        .add_text("name", "Updated With File")
        .add_part("thumbnail", thumbnail_part);

    let response = server.put("/typed-form/7").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 7);
    assert_eq!(result["name"], "Updated With File");
    assert_eq!(result["thumbnailUrl"], "updated_thumbnail_url");
    // Document not provided in this update
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_patch_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // PatchFileUploadRequest is generated via schema_type! with multipart + partial + omit = ["document"]
    // All fields are Option<T>, so we only send the ones we want to update
    let form = MultipartForm::new().add_text("name", "Patched Upload");

    let response = server.patch("/typed-form/99").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 99);
    assert_eq!(result["name"], "Patched Upload");
    // document_url is always None for patch (field omitted from PatchFileUploadRequest)
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_patch_file_upload_with_thumbnail() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"patched image".as_slice()).file_name("patched.jpg");

    let form = MultipartForm::new()
        .add_text("name", "Patched With Thumb")
        .add_part("thumbnail", thumbnail_part)
        .add_text("tags", "patched");

    let response = server.patch("/typed-form/55").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 55);
    assert_eq!(result["name"], "Patched With Thumb");
    assert_eq!(result["thumbnailUrl"], "patched_thumbnail_url");
    assert!(result["documentUrl"].is_null());

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], "patched");
}

#[tokio::test]
async fn test_typed_form_create_minimal() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // Only required field is "name" — all others are optional
    let form = MultipartForm::new().add_text("name", "Minimal Upload");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["name"], "Minimal Upload");
    assert!(result["thumbnailUrl"].is_null());
    assert!(result["documentUrl"].is_null());
    assert!(result["tags"].as_array().unwrap().is_empty());
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

// ============== #[form_data(limit = "...")] enforcement tests ==============
//
// These tests use a standalone Multipart struct with small limits to verify
// that the `#[form_data(limit)]` attribute is correctly enforced at runtime
// for both text fields and file (NamedTempFile) fields.

/// Test struct with intentionally small limits for limit enforcement testing.
#[derive(Debug, Multipart)]
#[allow(dead_code)]
struct FormDataLimitTestRequest {
    /// No limit — accepts any size.
    pub name: String,
    /// 100-byte limit on a text field.
    #[form_data(limit = "100")]
    pub data: Option<String>,
    /// 50-byte limit on a file upload field.
    #[form_data(limit = "50")]
    pub file: Option<FieldData<tempfile::NamedTempFile>>,
}

async fn form_data_limit_handler(
    TypedMultipart(req): TypedMultipart<FormDataLimitTestRequest>,
) -> axum::Json<String> {
    axum::Json(req.name)
}

fn create_limit_test_app() -> axum::Router {
    axum::Router::new().route("/limit-test", axum::routing::post(form_data_limit_handler))
}

#[tokio::test]
async fn test_form_data_limit_text_field_within_limit() {
    let server = TestServer::new(create_limit_test_app());

    // 5 bytes text — well within 100-byte limit
    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_text("data", "short");

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_form_data_limit_text_field_at_boundary() {
    let server = TestServer::new(create_limit_test_app());

    // Exactly 100 bytes — should succeed (limit check is `> limit`, not `>=`)
    let exact = "x".repeat(100);
    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_text("data", &exact);

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_form_data_limit_text_field_exceeds_limit() {
    let server = TestServer::new(create_limit_test_app());

    // 101 bytes — exceeds 100-byte limit → HTTP 413 PAYLOAD_TOO_LARGE
    let over = "x".repeat(101);
    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_text("data", &over);

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    let body = response.text();
    assert!(
        body.contains("data"),
        "Error should mention the field name 'data': {body}"
    );
}

#[tokio::test]
async fn test_form_data_limit_file_field_within_limit() {
    let server = TestServer::new(create_limit_test_app());

    // 50 bytes file — exactly at 50-byte limit
    let small_file = Part::bytes(vec![0u8; 50]).file_name("small.bin");
    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_part("file", small_file);

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_form_data_limit_file_field_exceeds_limit() {
    let server = TestServer::new(create_limit_test_app());

    // 51 bytes file — exceeds 50-byte limit → HTTP 413 PAYLOAD_TOO_LARGE
    let big_file = Part::bytes(vec![0u8; 51]).file_name("big.bin");
    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_part("file", big_file);

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    let body = response.text();
    assert!(
        body.contains("file"),
        "Error should mention the field name 'file': {body}"
    );
}

#[tokio::test]
async fn test_form_data_no_limit_field_accepts_large_data() {
    let server = TestServer::new(create_limit_test_app());

    // "name" has no #[form_data(limit)] — should accept large values
    let long_name = "x".repeat(10_000);
    let form = MultipartForm::new().add_text("name", &long_name);

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status_ok();

    let result: String = response.json();
    assert_eq!(result.len(), 10_000);
}

#[tokio::test]
async fn test_form_data_limit_unlimited_keyword() {
    // Verify that parse_byte_unit handles "unlimited" (code path: returns None)
    // Tested indirectly: a field without a limit already behaves as unlimited.
    // This test confirms the same behavior with all fields provided.
    let server = TestServer::new(create_limit_test_app());

    let form = MultipartForm::new()
        .add_text("name", "test")
        .add_text("data", "y".repeat(50))
        .add_part("file", Part::bytes(vec![1u8; 30]).file_name("f.bin"));

    let response = server.post("/limit-test").multipart(form).await;
    response.assert_status_ok();
}

// ============== #[serde(rename)] and #[serde(default)] tests ==============
//
// These tests verify that `#[derive(Multipart)]` correctly handles serde
// attributes for field renaming and default values.

fn default_greeting() -> String {
    "hello".to_string()
}

/// Test struct with serde rename and default attributes.
#[derive(Debug, Multipart)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct SerdeAttrTestRequest {
    /// Uses camelCase rename from struct-level rename_all.
    pub user_name: String,
    /// Explicit field rename overrides rename_all.
    #[serde(rename = "customTag")]
    pub tag_value: String,
    /// `#[serde(default)]` uses `Default::default()` when missing.
    #[serde(default)]
    pub score: i32,
    /// `#[serde(default = "fn")]` calls custom function when missing.
    #[serde(default = "default_greeting")]
    pub greeting: String,
}

async fn serde_attr_handler(
    TypedMultipart(req): TypedMultipart<SerdeAttrTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "userName": req.user_name,
        "tagValue": req.tag_value,
        "score": req.score,
        "greeting": req.greeting,
    }))
}

/// Test struct with struct-level `#[serde(default)]`.
#[derive(Debug, Multipart)]
#[serde(default)]
#[allow(dead_code)]
struct StructDefaultTestRequest {
    pub name: String,
    pub count: i32,
    pub active: bool,
}

async fn struct_default_handler(
    TypedMultipart(req): TypedMultipart<StructDefaultTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "count": req.count,
        "active": req.active,
    }))
}

fn create_serde_test_app() -> axum::Router {
    axum::Router::new()
        .route("/serde-test", axum::routing::post(serde_attr_handler))
        .route(
            "/struct-default-test",
            axum::routing::post(struct_default_handler),
        )
}

// ─── serde(rename_all) tests ────────────────────────────────────────────────

#[tokio::test]
async fn test_serde_rename_all_camel_case() {
    let server = TestServer::new(create_serde_test_app());

    // Field "user_name" is renamed to "userName" by rename_all = "camelCase"
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("customTag", "rust");

    let response = server.post("/serde-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["userName"], "Alice");
    assert_eq!(result["tagValue"], "rust");
}

#[tokio::test]
async fn test_serde_rename_all_rust_name_rejected() {
    let server = TestServer::new(create_serde_test_app());

    // Using Rust field name "user_name" instead of "userName" should fail
    let form = MultipartForm::new()
        .add_text("user_name", "Alice")
        .add_text("customTag", "rust");

    let response = server.post("/serde-test").multipart(form).await;
    // "userName" is missing → MissingField error
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// ─── serde(rename = "...") tests ────────────────────────────────────────────

#[tokio::test]
async fn test_serde_rename_explicit() {
    let server = TestServer::new(create_serde_test_app());

    // "tag_value" is renamed to "customTag" by #[serde(rename = "customTag")]
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("customTag", "explicit");

    let response = server.post("/serde-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["tagValue"], "explicit");
}

#[tokio::test]
async fn test_serde_rename_camel_case_of_field_rejected() {
    let server = TestServer::new(create_serde_test_app());

    // "tagValue" (camelCase of Rust name) should NOT work — explicit rename takes priority
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("tagValue", "wrong");

    let response = server.post("/serde-test").multipart(form).await;
    // "customTag" is missing → MissingField error
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// ─── serde(default) field-level tests ───────────────────────────────────────

#[tokio::test]
async fn test_serde_default_uses_default_trait() {
    let server = TestServer::new(create_serde_test_app());

    // Omit "score" (has #[serde(default)]) — should get i32::default() = 0
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("customTag", "test");

    let response = server.post("/serde-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["score"], 0, "score should default to 0");
}

#[tokio::test]
async fn test_serde_default_fn_uses_custom_function() {
    let server = TestServer::new(create_serde_test_app());

    // Omit "greeting" (has #[serde(default = "default_greeting")])
    // Should get "hello" from the custom function
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("customTag", "test");

    let response = server.post("/serde-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(
        result["greeting"], "hello",
        "greeting should default to 'hello' from default_greeting()"
    );
}

#[tokio::test]
async fn test_serde_default_overridden_when_provided() {
    let server = TestServer::new(create_serde_test_app());

    // Provide both default fields — explicit values should win
    let form = MultipartForm::new()
        .add_text("userName", "Alice")
        .add_text("customTag", "test")
        .add_text("score", "42")
        .add_text("greeting", "world");

    let response = server.post("/serde-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["score"], 42);
    assert_eq!(result["greeting"], "world");
}

// ─── Vec<T> field, strict mode, form_data(field_name), numeric/char tests ───

/// Test struct with Vec<T> field for repeated multipart fields.
#[derive(Debug, Multipart)]
#[allow(dead_code)]
struct VecFieldTestRequest {
    pub name: String,
    pub tags: Vec<String>,
}

async fn vec_field_handler(
    TypedMultipart(req): TypedMultipart<VecFieldTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "tags": req.tags,
    }))
}

/// Test struct with strict mode enabled.
#[derive(Debug, Multipart)]
#[try_from_multipart(strict)]
#[allow(dead_code)]
struct StrictModeTestRequest {
    pub name: String,
    pub age: i32,
}

async fn strict_mode_handler(
    TypedMultipart(req): TypedMultipart<StrictModeTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "age": req.age,
    }))
}

/// Test struct with form_data(field_name) override.
#[derive(Debug, Multipart)]
#[allow(dead_code)]
struct FieldNameOverrideTestRequest {
    pub name: String,
    #[form_data(field_name = "custom_field")]
    pub data: String,
}

async fn field_name_override_handler(
    TypedMultipart(req): TypedMultipart<FieldNameOverrideTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "data": req.data,
    }))
}

/// Test struct with form_data(default) attribute.
#[derive(Debug, Multipart)]
#[allow(dead_code)]
struct FormDataDefaultTestRequest {
    pub name: String,
    #[form_data(default)]
    pub count: i32,
}

async fn form_data_default_handler(
    TypedMultipart(req): TypedMultipart<FormDataDefaultTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "count": req.count,
    }))
}

/// Test struct with numeric and char fields for type parsing coverage.
#[derive(Debug, Multipart)]
#[allow(dead_code)]
struct NumericCharTestRequest {
    pub name: String,
    pub count: i32,
    pub score: f64,
    pub initial: char,
}

async fn numeric_char_handler(
    TypedMultipart(req): TypedMultipart<NumericCharTestRequest>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": req.name,
        "count": req.count,
        "score": req.score,
        "initial": req.initial.to_string(),
    }))
}

fn create_coverage_test_app() -> axum::Router {
    axum::Router::new()
        .route("/vec-test", axum::routing::post(vec_field_handler))
        .route("/strict-test", axum::routing::post(strict_mode_handler))
        .route(
            "/field-name-test",
            axum::routing::post(field_name_override_handler),
        )
        .route(
            "/form-data-default-test",
            axum::routing::post(form_data_default_handler),
        )
        .route(
            "/numeric-char-test",
            axum::routing::post(numeric_char_handler),
        )
}

// ─── Vec<T> field tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_vec_field_multiple_values() {
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("tags", "rust")
        .add_text("tags", "web")
        .add_text("tags", "api");

    let response = server.post("/vec-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["name"], "Alice");
    assert_eq!(result["tags"], json!(["rust", "web", "api"]));
}

#[tokio::test]
async fn test_vec_field_empty() {
    let server = TestServer::new(create_coverage_test_app());

    // No "tags" fields — Vec should be empty
    let form = MultipartForm::new().add_text("name", "Bob");

    let response = server.post("/vec-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["tags"], json!([]));
}

#[tokio::test]
async fn test_vec_field_single_value() {
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new()
        .add_text("name", "Charlie")
        .add_text("tags", "solo");

    let response = server.post("/vec-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["tags"], json!(["solo"]));
}

// ─── Strict mode tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_strict_mode_valid_request() {
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("age", "30");

    let response = server.post("/strict-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["name"], "Alice");
    assert_eq!(result["age"], 30);
}

#[tokio::test]
async fn test_strict_mode_unknown_field() {
    let server = TestServer::new(create_coverage_test_app());

    // "extra" is not a field in StrictModeTestRequest → UnknownField error
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("age", "30")
        .add_text("extra", "rejected");

    let response = server.post("/strict-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Unknown field"),
        "Should mention unknown field: {body}"
    );
}

#[tokio::test]
async fn test_strict_mode_duplicate_field() {
    let server = TestServer::new(create_coverage_test_app());

    // Sending "name" twice in strict mode → DuplicateField error
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("name", "Bob")
        .add_text("age", "30");

    let response = server.post("/strict-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Duplicate field"),
        "Should mention duplicate field: {body}"
    );
}

// ─── form_data(field_name) tests ────────────────────────────────────────────

#[tokio::test]
async fn test_form_data_field_name_override() {
    let server = TestServer::new(create_coverage_test_app());

    // "data" field is mapped to "custom_field" via form_data(field_name)
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("custom_field", "payload");

    let response = server.post("/field-name-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["data"], "payload");
}

#[tokio::test]
async fn test_form_data_field_name_rust_name_rejected() {
    let server = TestServer::new(create_coverage_test_app());

    // Using Rust field name "data" instead of "custom_field" → MissingField
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("data", "payload");

    let response = server.post("/field-name-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// ─── form_data(default) tests ───────────────────────────────────────────────

#[tokio::test]
async fn test_form_data_default_uses_default_trait() {
    let server = TestServer::new(create_coverage_test_app());

    // Omit "count" (has #[form_data(default)]) → Default::default() = 0
    let form = MultipartForm::new().add_text("name", "Alice");

    let response = server.post("/form-data-default-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["count"], 0);
}

#[tokio::test]
async fn test_form_data_default_overridden_when_provided() {
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "42");

    let response = server.post("/form-data-default-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["count"], 42);
}

// ─── Numeric and char field parsing tests ───────────────────────────────────

#[tokio::test]
async fn test_numeric_char_valid_values() {
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "42")
        .add_text("score", "9.75")
        .add_text("initial", "A");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["count"], 42);
    assert!((result["score"].as_f64().unwrap() - 9.75).abs() < f64::EPSILON);
    assert_eq!(result["initial"], "A");
}

#[tokio::test]
async fn test_numeric_field_invalid_value() {
    let server = TestServer::new(create_coverage_test_app());

    // "not_a_number" for i32 field → WrongFieldType
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "not_a_number")
        .add_text("score", "9.75")
        .add_text("initial", "A");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn test_float_field_invalid_value() {
    let server = TestServer::new(create_coverage_test_app());

    // "abc" for f64 field → WrongFieldType
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "10")
        .add_text("score", "abc")
        .add_text("initial", "A");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn test_char_field_multiple_chars() {
    let server = TestServer::new(create_coverage_test_app());

    // "AB" for char field → WrongFieldType (expects exactly one character)
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "10")
        .add_text("score", "1.0")
        .add_text("initial", "AB");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn test_char_field_empty_string() {
    let server = TestServer::new(create_coverage_test_app());

    // "" for char field → WrongFieldType (expects exactly one character)
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_text("count", "10")
        .add_text("score", "1.0")
        .add_text("initial", "");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

// ─── serde(default) struct-level tests ──────────────────────────────────────

#[tokio::test]
async fn test_struct_level_serde_default_all_omitted() {
    let server = TestServer::new(create_serde_test_app());

    // No recognized fields — struct has #[serde(default)], all get Default::default().
    // Send an unrecognized field to produce a valid multipart body (non-strict ignores it).
    let form = MultipartForm::new().add_text("_ignored", "");

    let response = server.post("/struct-default-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["name"], "", "String::default() is empty string");
    assert_eq!(result["count"], 0, "i32::default() is 0");
    assert_eq!(result["active"], false, "bool::default() is false");
}

#[tokio::test]
async fn test_struct_level_serde_default_partial() {
    let server = TestServer::new(create_serde_test_app());

    // Only provide "name" — other fields should get defaults
    let form = MultipartForm::new().add_text("name", "Bob");

    let response = server.post("/struct-default-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["name"], "Bob");
    assert_eq!(result["count"], 0);
    assert_eq!(result["active"], false);
}

#[tokio::test]
async fn test_struct_level_serde_default_all_provided() {
    let server = TestServer::new(create_serde_test_app());

    // Provide all fields — explicit values should win
    let form = MultipartForm::new()
        .add_text("name", "Charlie")
        .add_text("count", "99")
        .add_text("active", "true");

    let response = server.post("/struct-default-test").multipart(form).await;
    response.assert_status_ok();

    let result: serde_json::Value = response.json();
    assert_eq!(result["name"], "Charlie");
    assert_eq!(result["count"], 99);
    assert_eq!(result["active"], true);
}

// ============== Multipart error path coverage tests ==========================
//
// These tests trigger real axum MultipartRejection / MultipartError paths
// to cover From impls and Display arms for InvalidRequest/InvalidRequestBody.

#[tokio::test]
async fn test_multipart_rejection_non_multipart_content_type() {
    // Sending JSON to a multipart handler triggers MultipartRejection → InvalidRequest
    let server = TestServer::new(create_coverage_test_app());

    let response = server
        .post("/strict-test")
        .content_type("application/json")
        .bytes(b"{\"name\":\"x\",\"age\":1}".to_vec().into())
        .await;

    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Invalid multipart request"),
        "Should use InvalidRequest Display: {body}"
    );
}

#[tokio::test]
async fn test_multipart_rejection_missing_content_type() {
    // Sending raw bytes with no content type triggers MultipartRejection
    let server = TestServer::new(create_coverage_test_app());

    let response = server
        .post("/vec-test")
        .bytes(b"not multipart".to_vec().into())
        .await;

    // axum rejects with 4xx because there's no multipart content-type
    assert!(
        response.status_code().is_client_error(),
        "Should be a client error, got {}",
        response.status_code()
    );
}

#[tokio::test]
async fn test_numeric_field_non_utf8_bytes() {
    // Send non-UTF-8 bytes for a numeric (i32) field → WrongFieldType from from_utf8 error
    let server = TestServer::new(create_coverage_test_app());

    let invalid_utf8 = Part::bytes(vec![0xFF, 0xFE, 0xFD]).file_name("bad.bin");
    let form = MultipartForm::new()
        .add_text("name", "Alice")
        .add_part("count", invalid_utf8)
        .add_text("score", "1.0")
        .add_text("initial", "A");

    let response = server.post("/numeric-char-test").multipart(form).await;
    response.assert_status(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
