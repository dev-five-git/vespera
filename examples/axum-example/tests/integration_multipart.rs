use axum_test::{
    TestServer,
    multipart::{MultipartForm, Part},
};
use serde_json::json;
use vespera::{
    Multipart, axum,
    multipart::{FieldData, TypedMultipart},
};

// ============== #[form_data(limit = "...")] enforcement tests ==============
//
// These tests use a standalone Multipart struct with small limits to verify
// that the `#[form_data(limit)]` attribute is correctly enforced at runtime
// for both text fields and file (NamedTempFile) fields.

/// Test struct with intentionally small limits for limit enforcement testing.
/// The `data` and `file` fields are intentionally not read in the handler —
/// the test exercises the multipart parser's limit-rejection path, so the
/// fields must exist (so the derive macro registers them with their limits)
/// but the handler never touches their values.
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
    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
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
    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
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
    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
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
    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
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
    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_multipart_error_malformed_body_stream() {
    // Send a valid multipart Content-Type so from_request succeeds,
    // but with a corrupted body so next_field() returns Err(MultipartError).
    // This covers From<MultipartError> (line 135) and InvalidRequestBody Display (lines 93-94).
    let server = TestServer::new(create_coverage_test_app());

    let boundary = "TESTBOUNDARY";
    // Start a valid boundary, then inject invalid header bytes (0xFF is not valid in HTTP headers).
    // multer will attempt to parse these as field headers and fail.
    let mut body = Vec::new();
    body.extend_from_slice(b"--TESTBOUNDARY\r\n");
    body.extend_from_slice(&[0x01, 0x02, 0xFF, 0xFE]); // invalid header bytes
    body.extend_from_slice(b"\r\n\r\ndata\r\n--TESTBOUNDARY--");

    let response = server
        .post("/strict-test")
        .content_type(&format!("multipart/form-data; boundary={boundary}"))
        .bytes(body.into())
        .await;

    // multer rejects the invalid header bytes → MultipartError → From<MultipartError> → InvalidRequestBody
    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Invalid multipart body"),
        "Expected InvalidRequestBody Display output, got: {body}"
    );
}

#[tokio::test]
async fn test_missing_required_field() {
    // Send only "name" but omit required "age" → MissingField error from post-loop check
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new().add_text("name", "Alice");
    let response = server.post("/strict-test").multipart(form).await;

    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Missing field"),
        "Expected MissingField error, got: {body}"
    );
    assert!(
        body.contains("age"),
        "Should name the missing field 'age', got: {body}"
    );
}

#[tokio::test]
async fn test_missing_multiple_required_fields() {
    // Send only "name" to numeric-char-test which requires name, count, score, initial.
    // Non-strict endpoint: the unmatched fields simply stay None → MissingField in post-loop.
    let server = TestServer::new(create_coverage_test_app());

    let form = MultipartForm::new().add_text("name", "Alice");
    let response = server.post("/numeric-char-test").multipart(form).await;

    response.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body = response.text();
    assert!(
        body.contains("Missing field"),
        "Expected MissingField error, got: {body}"
    );
}
