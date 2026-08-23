use axum_example::create_app;
use axum_test::TestServer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vespera::{Schema, schema};

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

    // Required is nullability-only, matching the OpenAPI component schema:
    // 'id'/'name' are non-Option, and 'bio' is non-Option too — its
    // `#[serde(default)]` does NOT exclude it from `required`. Only 'email'
    // (Option<T>) is optional. (`schema!` now shares the OpenAPI generation
    // path, so it no longer diverges by dropping defaulted fields.)
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
    assert!(
        !required.contains(&"email".to_string()),
        "'email' is Option<T>, should not be required"
    );
    assert!(
        required.contains(&"bio".to_string()),
        "'bio' is non-Option; #[serde(default)] does not affect required \
         status (required is nullability-only, matching OpenAPI)"
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

#[tokio::test]
async fn test_memo_detail_same_file_relation_adapter_runtime_shape() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/memos/9/detail").await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 9);
    assert_eq!(result["title"], "Detailed Memo");
    assert_eq!(result["user"]["id"], 7);
    assert_eq!(result["user"]["email"], "memo@example.com");
    assert_eq!(result["user"]["name"], "Memo User");
    assert!(result["user"].get("createdAt").is_none());
    assert!(result["user"].get("updatedAt").is_none());

    let comments = result["memoComments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["memoId"], 9);
    assert_eq!(comments[0]["content"], "Looks good");
}

#[test]
fn decimal_serializes_as_string_at_runtime() {
    // `rust_decimal`'s serde serializes `Decimal` as a JSON STRING (to preserve
    // precision), so the OpenAPI mapping for `Decimal` must be
    // `{type:string, format:decimal}`, not `number`. Locks that assumption so
    // the spec cannot silently regress to lying about the wire type.
    let value = serde_json::to_value(sea_orm::prelude::Decimal::new(1050, 2)).unwrap();
    assert!(
        value.is_string(),
        "Decimal serialized as {value:?}, expected a JSON string"
    );
    assert_eq!(value, serde_json::json!("10.50"));
}
