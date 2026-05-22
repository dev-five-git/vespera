//! End-to-end test: `Validated<Json<T>>` axum extractor rejects invalid
//! payloads with `422 Unprocessable Entity` + a JSON error envelope, and
//! lets valid payloads through to the handler.

#![cfg(feature = "validation")]

use ::axum::{Router, body::Body, http::Request, routing::post};
use ::serde::Deserialize;
use ::tower::ServiceExt;
use ::vespera::{Schema, Validated};

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct CreatePost {
    #[schema(min_length = 3, max_length = 50)]
    title: String,

    #[schema(min_length = 1)]
    content: String,
}

async fn create_post(
    Validated(::axum::Json(_payload)): Validated<::axum::Json<CreatePost>>,
) -> &'static str {
    "ok"
}

fn router() -> Router {
    Router::new().route("/posts", post(create_post))
}

async fn body_to_string(body: Body) -> String {
    let bytes = ::axum::body::to_bytes(body, usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn valid_payload_returns_200() {
    let app = router();
    let req = Request::builder()
        .method("POST")
        .uri("/posts")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"title":"My Post","content":"hello world"}"#,
        ))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(body_to_string(res.into_body()).await, "ok");
}

#[tokio::test]
async fn short_title_returns_422_with_path_keyed_envelope() {
    let app = router();
    let req = Request::builder()
        .method("POST")
        .uri("/posts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"title":"X","content":"ok"}"#))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);
    assert_eq!(
        res.headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        Some("application/json"),
    );

    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();

    let errors = body["errors"].as_array().expect("errors array missing");
    assert!(!errors.is_empty(), "errors array is empty");
    assert!(
        errors
            .iter()
            .any(|e| e["path"].as_str() == Some("title")
                && e["message"].as_str().is_some()),
        "expected an error with path=\"title\", got {body:#}"
    );
}

#[tokio::test]
async fn empty_content_returns_422() {
    let app = router();
    let req = Request::builder()
        .method("POST")
        .uri("/posts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"title":"Valid title","content":""}"#))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);

    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().unwrap();
    assert!(errors.iter().any(|e| e["path"].as_str() == Some("content")));
}

#[tokio::test]
async fn multiple_violations_all_appear_in_envelope() {
    let app = router();
    let req = Request::builder()
        .method("POST")
        .uri("/posts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"title":"X","content":""}"#))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);

    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().unwrap();
    let paths: Vec<&str> = errors
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(paths.contains(&"title"), "got {paths:?}");
    assert!(paths.contains(&"content"), "got {paths:?}");
}

#[tokio::test]
async fn malformed_json_propagates_400_not_422() {
    // When the inner extractor itself fails (e.g. invalid JSON),
    // `Validated<T>` must forward that rejection unchanged rather than
    // synthesizing a 422 from a non-existent garde report.
    let app = router();
    let req = Request::builder()
        .method("POST")
        .uri("/posts")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    // Axum's Json extractor returns 400 (or 415 depending on cause) —
    // anything that is NOT our 422 envelope is acceptable here.
    assert_ne!(res.status(), 422);
}

// ── per-rule 422 coverage ────────────────────────────────────────────
//
// `CreatePost` only exercises `min_length` / `max_length`.  The model
// below pulls in every other rule we emit so each runs through the
// full extractor → garde → 422 envelope flow at least once.

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct AllRules {
    /// String length + pattern + format.
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    username: String,

    /// `format = "email"` → garde `email::apply`.
    #[schema(format = "email")]
    email: String,

    /// `format = "uri"` → garde `url::apply`.
    #[schema(format = "uri")]
    homepage: String,

    /// `format = "ipv4"` → garde `ip::apply(IpKind::V4)`.
    #[schema(format = "ipv4")]
    addr_v4: String,

    /// `format = "ipv6"` → garde `ip::apply(IpKind::V6)`.
    #[schema(format = "ipv6")]
    addr_v6: String,

    /// Numeric range.
    #[schema(minimum = 0, maximum = 150)]
    age: u32,

    /// `Vec` length + uniqueness annotation (uniqueness itself is
    /// OpenAPI-only — no garde rule).
    #[schema(min_items = 1, max_items = 3, unique_items)]
    tags: Vec<String>,

    /// `Option<T>` field — should validate only when `Some`.
    #[schema(min_length = 8)]
    nickname: Option<String>,
}

async fn create_all_rules(
    Validated(::axum::Json(_p)): Validated<::axum::Json<AllRules>>,
) -> &'static str {
    "ok"
}

fn all_rules_router() -> Router {
    Router::new().route("/all", post(create_all_rules))
}

fn good_payload() -> ::serde_json::Value {
    ::serde_json::json!({
        "username":  "alice_99",
        "email":     "alice@example.com",
        "homepage":  "https://alice.example.com",
        "addr_v4":   "192.168.0.1",
        "addr_v6":   "::1",
        "age":       30,
        "tags":      ["a", "b"],
        "nickname":  null
    })
}

/// Send `payload` to `/all` and decode the response as
/// `(status, body_json)`.  Asserts `application/json` content-type when
/// the status is `422` (the canonical validation envelope).
async fn dispatch(
    app: Router,
    payload: ::serde_json::Value,
) -> (u16, ::serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/all")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    if status == 422 {
        assert_eq!(
            res.headers().get("content-type").map(|v| v.to_str().unwrap()),
            Some("application/json"),
        );
    }
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await)
            .unwrap_or(::serde_json::Value::Null);
    (status, body)
}

/// Assert that `body` is the 422 envelope and contains at least one
/// error whose `path == field`.
fn assert_envelope_has_field_error(body: &::serde_json::Value, field: &str) {
    let errors = body["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("missing `errors` array in {body:#}"));
    assert!(
        errors
            .iter()
            .any(|e| e["path"].as_str() == Some(field)
                && e["message"].as_str().is_some()),
        "expected an error with path=\"{field}\" + message, got {body:#}",
    );
}

#[tokio::test]
async fn all_rules_happy_path_returns_200() {
    let (status, _) = dispatch(all_rules_router(), good_payload()).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn rule_pattern_violation_returns_422() {
    let mut bad = good_payload();
    bad["username"] = ::serde_json::json!("Alice99"); // uppercase fails `^[a-z0-9_]+$`
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "username");
}

#[tokio::test]
async fn rule_format_email_violation_returns_422() {
    let mut bad = good_payload();
    bad["email"] = ::serde_json::json!("not-an-email");
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "email");
}

#[tokio::test]
async fn rule_format_uri_violation_returns_422() {
    let mut bad = good_payload();
    bad["homepage"] = ::serde_json::json!("not a url");
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "homepage");
}

#[tokio::test]
async fn rule_format_ipv4_violation_returns_422() {
    let mut bad = good_payload();
    bad["addr_v4"] = ::serde_json::json!("999.999.999.999");
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "addr_v4");
}

#[tokio::test]
async fn rule_format_ipv6_violation_returns_422() {
    let mut bad = good_payload();
    bad["addr_v6"] = ::serde_json::json!("not-an-ipv6-address");
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "addr_v6");
}

#[tokio::test]
async fn rule_range_minimum_violation_returns_422() {
    // `age` lives in `u32`; the only sub-`minimum=0` value JSON can
    // express against a `u32` is via serde rejecting -1.  To exercise
    // the `range::apply` rule itself we use a type that allows a value
    // below the schema minimum on a fresh struct.
    #[derive(Deserialize, Schema)]
    #[allow(dead_code)]
    struct Signed {
        #[schema(minimum = 0, maximum = 150)]
        age: i32,
    }
    async fn handler(
        Validated(::axum::Json(_)): Validated<::axum::Json<Signed>>,
    ) -> &'static str {
        "ok"
    }
    let app = Router::new().route("/n", post(handler));
    let req = Request::builder()
        .method("POST")
        .uri("/n")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"age":-1}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    assert_envelope_has_field_error(&body, "age");
}

#[tokio::test]
async fn rule_range_maximum_violation_returns_422() {
    let mut bad = good_payload();
    bad["age"] = ::serde_json::json!(9999);
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "age");
}

#[tokio::test]
async fn rule_min_items_violation_returns_422() {
    let mut bad = good_payload();
    bad["tags"] = ::serde_json::json!([]); // empty Vec < min_items=1
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "tags");
}

#[tokio::test]
async fn rule_max_items_violation_returns_422() {
    let mut bad = good_payload();
    bad["tags"] = ::serde_json::json!(["a", "b", "c", "d"]); // 4 > max_items=3
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "tags");
}

#[tokio::test]
async fn rule_option_field_validates_only_when_some_returns_422() {
    let mut bad = good_payload();
    bad["nickname"] = ::serde_json::json!("hi"); // 2 chars < min_length=8
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    assert_envelope_has_field_error(&body, "nickname");
}

#[tokio::test]
async fn rule_option_field_none_skips_validation() {
    // `nickname: null` must not contribute a 422 — Option<T> validates
    // only when `Some`.  The rest of the payload is valid, so we
    // expect a clean 200.
    let mut p = good_payload();
    p["nickname"] = ::serde_json::Value::Null;
    let (status, _) = dispatch(all_rules_router(), p).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn multiple_per_rule_violations_all_appear_in_envelope() {
    let bad = ::serde_json::json!({
        "username":  "BAD!",                  // pattern + (length OK at 4)
        "email":     "broken",                // format=email
        "homepage":  "broken",                // format=uri
        "addr_v4":   "999.999.999.999",       // format=ipv4
        "addr_v6":   "broken",                // format=ipv6
        "age":       9999,                    // range
        "tags":      [],                      // min_items
        "nickname":  "x"                      // Option's min_length
    });
    let (status, body) = dispatch(all_rules_router(), bad).await;
    assert_eq!(status, 422);
    for field in [
        "username", "email", "homepage", "addr_v4", "addr_v6", "age", "tags",
        "nickname",
    ] {
        assert_envelope_has_field_error(&body, field);
    }
}
