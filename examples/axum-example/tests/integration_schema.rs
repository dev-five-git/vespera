use axum_example::create_app;
use serde::{Deserialize, Serialize};
use serde_json::json;
use testkit::TestServer;
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

#[derive(Serialize, Deserialize, Clone, Default, Schema)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    #[default]
    Active,
    Archived,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct TestWithRefDefault {
    #[serde(default)]
    #[schema(default = "active")]
    pub status: TestStatus,
}

/// An explicit `#[schema(default)]` on a `$ref`-typed field is documented
/// as a sibling of the `$ref` (OpenAPI 3.1), not dropped.
#[test]
fn test_schema_macro_documents_ref_default() {
    let schema = schema!(TestWithRefDefault);
    let status = serde_json::to_value(&schema.properties.unwrap()["status"]).unwrap();
    assert_eq!(
        status,
        json!({ "$ref": "#/components/schemas/TestStatus", "default": "active" })
    );
}

/// `schema_type!` of a cross-file model whose `DateTimeLocal` field has a
/// `NOW()` default compiles (no OpenAPI default is advertised for a
/// machine-dependent value) and serde fills the omitted field with the epoch.
#[tokio::test]
async fn test_schema_type_local_now_default() {
    let server = TestServer::new(create_app().await);
    let response = server
        .post("/uuid-items/clock")
        .json(&json!({ "id": 7 }))
        .await;
    response.assert_status_ok();
    let clock: serde_json::Value = response.json();
    assert_eq!(clock["id"], 7);
    let created_at = clock["createdAt"].as_str().unwrap();
    let parsed = vespera::chrono::DateTime::parse_from_rfc3339(created_at).unwrap();
    assert_eq!(parsed.timestamp(), 0, "{created_at}");
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
    // The component schema's `default` too: `schema!` applies the same
    // default post-processing.
    let vespera::schema::SchemaRef::Inline(bio) = &properties["bio"] else {
        panic!("inline bio");
    };
    assert_eq!(bio.default, Some(json!("")));

    // `schema!` is the response view, matching the OpenAPI component
    // schema: serde writes every field, so all are required — 'bio' despite
    // its `#[serde(default)]` (which only matters when reading), and the
    // Option<T> 'email' too (written as `null`, its schema is nullable).
    let required = user_schema.required.unwrap();
    assert!(required.contains(&"id".to_string()));
    assert!(required.contains(&"name".to_string()));
    assert!(
        required.contains(&"email".to_string()),
        "'email' is Option<T>: always written (as null), so required"
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

    // Verify the Rust field names appear in no spelling: `UserDTO` is
    // camelCase, so a lost rename would surface as `userId` / `displayName`.
    for key in ["user_id", "display_name", "userId", "displayName"] {
        assert!(
            user.get(key).is_none(),
            "'{key}' should not appear in JSON: {user}"
        );
    }
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
    // The DTO serializes camelCase (`schema_type!`'s default), so an
    // un-omitted field would appear as `createdAt`.
    assert!(
        result.get("createdAt").is_none() && result.get("created_at").is_none(),
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

/// A response type mixing every `Option` / skip combination.
#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(rename_all = "camelCase")]
pub struct WireProfile {
    pub name: String,
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing)]
    pub secret: String,
}

/// The response schema matches what axum's `Json` actually sends: a `None`
/// is sent as `"nickname": null` (so `nickname` is required and nullable),
/// a `skip_serializing_if` `None` is left out (not required), and a
/// `skip_serializing` field is never sent (not a property).
#[tokio::test]
async fn response_schema_matches_the_serialized_body() {
    use vespera::axum::{Json, Router, routing::get};

    let app = Router::new().route(
        "/profile",
        get(|| async {
            Json(WireProfile {
                name: "n".to_string(),
                nickname: None,
                email: None,
                secret: "s".to_string(),
            })
        }),
    );
    let body: serde_json::Value = TestServer::new(app).get("/profile").await.json();
    assert_eq!(body, json!({"name": "n", "nickname": null}));

    let schema = serde_json::to_value(schema!(WireProfile)).unwrap();
    assert_eq!(schema["required"], json!(["name", "nickname"]));
    assert_eq!(
        schema["properties"]["nickname"]["type"],
        json!(["string", "null"])
    );
    assert!(schema["properties"].get("email").is_some());
    assert!(schema["properties"].get("secret").is_none());
    // Every required key is really in the body.
    for key in schema["required"].as_array().unwrap() {
        assert!(body.get(key.as_str().unwrap()).is_some(), "{key} missing");
    }
}
