//! Exercise the [`vespera::Validated`] extractor against axum's
//! [`Form`], [`Query`], and [`Path`] payloads — the three
//! `ValidatePayload` impls that aren't covered by the JSON-only
//! integration test in `validated_extractor.rs`.
//!
//! Each test drives a real `Router::oneshot` round-trip so the
//! `Validated<...>` extraction path runs end-to-end, including the
//! 422 envelope on validation failure and pass-through on success.

#![cfg(feature = "validation")]

use ::axum::{
    Form, Router,
    body::Body,
    extract::{Path, Query},
    http::Request,
    routing::{get, post},
};
use ::serde::Deserialize;
use ::tower::ServiceExt;
use ::vespera::{Schema, ValidatePayload, Validated, ValidatedWith};

async fn body_to_string(body: Body) -> String {
    let bytes = ::axum::body::to_bytes(body, usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ── Form ─────────────────────────────────────────────────────────────

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct LoginForm {
    #[schema(min_length = 3, max_length = 32)]
    username: String,

    #[schema(min_length = 8)]
    password: String,
}

async fn login(Validated(Form(_p)): Validated<Form<LoginForm>>) -> &'static str {
    "ok"
}

fn form_router() -> Router {
    Router::new().route("/login", post(login))
}

#[tokio::test]
async fn validated_form_valid_payload_returns_200() {
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("username=alice&password=correcthorse"))
        .unwrap();
    let res = form_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn validated_form_short_password_returns_422() {
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("username=alice&password=short"))
        .unwrap();
    let res = form_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().expect("errors");
    assert!(
        errors
            .iter()
            .any(|e| e["path"].as_str() == Some("password")),
        "expected `password` error, got {body:#}"
    );
}

// ── Query / Path handler arguments ───────────────────────────────────
//
// Axum's `Query<T>` and `Path<T>` impl `FromRequestParts` (not
// `FromRequest` directly). `Validated<Query<T>>` / `Validated<Path<T>>`
// therefore exercise `Validated`'s `FromRequestParts` impl when installed
// as handler arguments, including alongside a body extractor.

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct SearchParams {
    #[schema(min_length = 1, max_length = 100)]
    q: String,
    #[schema(minimum = 1, maximum = 100)]
    limit: u32,
}

async fn search(Validated(Query(_q)): Validated<Query<SearchParams>>) -> &'static str {
    "ok"
}

fn query_router() -> Router {
    Router::new().route("/search", get(search))
}

#[tokio::test]
async fn validated_query_handler_arg_valid_payload_returns_200() {
    let req = Request::builder()
        .method("GET")
        .uri("/search?q=hello&limit=10")
        .body(Body::empty())
        .unwrap();
    let res = query_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn validated_query_handler_arg_invalid_payload_returns_422() {
    let req = Request::builder()
        .method("GET")
        .uri("/search?q=&limit=999")
        .body(Body::empty())
        .unwrap();
    let res = query_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().expect("errors");
    assert!(
        errors.iter().any(|e| e["path"].as_str() == Some("q")),
        "expected `q` error, got {body:#}"
    );
}

#[test]
fn validated_query_payload_exposes_inner_ref() {
    use ::garde::Validate;
    let q = Query(SearchParams {
        q: "hello".into(),
        limit: 10,
    });
    let inner: &SearchParams = ValidatePayload::payload(&q);
    assert!(inner.validate().is_ok());
    assert_eq!(inner.q, "hello");
    assert_eq!(inner.limit, 10);
}

#[test]
fn validated_query_payload_invalid_inner_value_is_rejected_by_garde() {
    use ::garde::Validate;
    let q = Query(SearchParams {
        q: String::new(), // fails min_length=1
        limit: 999,       // fails maximum=100
    });
    let inner: &SearchParams = ValidatePayload::payload(&q);
    let err = inner.validate().expect_err("invalid query rejects");
    let paths: Vec<String> = err.iter().map(|(p, _)| p.to_string()).collect();
    assert!(paths.contains(&"q".to_string()));
    assert!(paths.contains(&"limit".to_string()));
}

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct UserPath {
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    username: String,
}

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct UpdateBody {
    #[schema(min_length = 3, max_length = 32)]
    display_name: String,
}

async fn update_user(
    Validated(Path(_path)): Validated<Path<UserPath>>,
    Validated(::axum::Json(_body)): Validated<::axum::Json<UpdateBody>>,
) -> &'static str {
    "ok"
}

fn path_and_body_router() -> Router {
    Router::new().route("/users/{username}", post(update_user))
}

#[tokio::test]
async fn validated_path_handler_arg_can_coexist_with_body_extractor() {
    let req = Request::builder()
        .method("POST")
        .uri("/users/alice_99")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"display_name":"Alice"}"#))
        .unwrap();
    let res = path_and_body_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn validated_path_handler_arg_invalid_payload_returns_422() {
    let req = Request::builder()
        .method("POST")
        .uri("/users/BAD")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"display_name":"Alice"}"#))
        .unwrap();
    let res = path_and_body_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 422);
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().expect("errors");
    assert!(
        errors
            .iter()
            .any(|e| e["path"].as_str() == Some("username")),
        "expected `username` error, got {body:#}"
    );
}

#[test]
fn validated_path_payload_exposes_inner_ref() {
    use ::garde::Validate;
    let p = Path(UserPath {
        username: "alice_99".into(),
    });
    let inner: &UserPath = ValidatePayload::payload(&p);
    assert!(inner.validate().is_ok());
}

#[test]
fn validated_path_payload_invalid_inner_value_is_rejected_by_garde() {
    use ::garde::Validate;
    let p = Path(UserPath {
        username: "BAD".into(), // uppercase fails the lowercase pattern
    });
    let inner: &UserPath = ValidatePayload::payload(&p);
    let err = inner.validate().expect_err("uppercase rejects");
    let paths: Vec<String> = err.iter().map(|(p, _)| p.to_string()).collect();
    assert!(paths.contains(&"username".to_string()));
}

// ── Form payload() impl (same shape, end-to-end via Router) ──────────

#[test]
fn validated_form_payload_exposes_inner_ref() {
    use ::garde::Validate;
    let f = Form(LoginForm {
        username: "alice".into(),
        password: "correcthorse".into(),
    });
    let inner: &LoginForm = ValidatePayload::payload(&f);
    assert!(inner.validate().is_ok());
}

// ── Inner extractor failures propagate unchanged ─────────────────────

#[tokio::test]
async fn validated_form_inner_extractor_rejection_is_forwarded() {
    // Missing content-type → axum's Form extractor rejects with 415
    // and our `Validated` wrapper must forward that response verbatim.
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .body(Body::from("username=alice&password=correcthorse"))
        .unwrap();
    let res = form_router().oneshot(req).await.unwrap();
    assert_ne!(res.status(), 422, "must not synthesize a 422 envelope");
    assert_ne!(res.status(), 200, "must not pass through to handler");
}

#[derive(Clone)]
struct PrefixContext {
    required_prefix: String,
}

#[derive(Deserialize, garde::Validate)]
#[garde(context(PrefixContext as ctx))]
struct ContextSearch {
    #[garde(custom(|value: &str, ctx: &PrefixContext| {
        if value.starts_with(&ctx.required_prefix) {
            Ok(())
        } else {
            Err(garde::Error::new("missing required prefix"))
        }
    }))]
    q: String,
}

async fn context_search(validated: ValidatedWith<PrefixContext, Query<ContextSearch>>) -> String {
    validated.get().0.q.clone()
}

fn context_query_router() -> Router<PrefixContext> {
    Router::new().route("/context-search", get(context_search))
}

#[tokio::test]
async fn context_validated_query_accepts_state_approved_value() {
    let app = context_query_router().with_state(PrefixContext {
        required_prefix: "vespera-".to_owned(),
    });
    let req = Request::builder()
        .uri("/context-search?q=vespera-release")
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), 200);
    assert_eq!(body_to_string(res.into_body()).await, "vespera-release");
}

#[tokio::test]
async fn context_validated_query_rejects_state_disapproved_value() {
    let app = context_query_router().with_state(PrefixContext {
        required_prefix: "vespera-".to_owned(),
    });
    let req = Request::builder()
        .uri("/context-search?q=other-release")
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), 422);
    let body: ::serde_json::Value =
        ::serde_json::from_str(&body_to_string(res.into_body()).await).unwrap();
    let errors = body["errors"].as_array().expect("errors");
    assert!(errors.iter().any(|error| error["path"] == "q"));
}
