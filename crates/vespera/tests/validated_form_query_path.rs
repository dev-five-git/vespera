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

// ── Deserialization (data) failures share the 422 envelope ───────────

#[derive(Deserialize, garde::Validate)]
struct ItemPath {
    #[garde(skip)]
    #[allow(dead_code)]
    id: u32,
}

async fn item(Validated(Path(_p)): Validated<Path<ItemPath>>) -> &'static str {
    "ok"
}

#[derive(Deserialize, garde::Validate)]
struct Page {
    #[garde(skip)]
    #[allow(dead_code)]
    page: u32,
}

async fn page(Validated(Query(_p)): Validated<Query<Page>>) -> &'static str {
    "ok"
}

fn data_error_router() -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/items/{id}", get(item))
        .route("/page", get(page))
}

#[tokio::test]
async fn deserialization_failures_use_the_422_envelope() {
    let form = |body: &'static str| {
        Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    };
    let get = |uri: &'static str| {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    };
    let cases = [
        ("form_missing_field", form("username=alice"), "password"),
        ("query_wrong_type", get("/page?page=abc"), "page"),
        ("query_missing_field", get("/page"), "page"),
        ("path_struct_wrong_type", get("/items/abc"), "id"),
    ];
    for (name, req, expected_path) in cases {
        let res = data_error_router().oneshot(req).await.unwrap();
        assert_eq!(res.status(), 422, "{name}");
        assert_eq!(
            res.headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("application/json"),
            "{name}"
        );
        let body = body_to_string(res.into_body()).await;
        let parsed: ::serde_json::Value = ::serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["errors"][0]["path"], expected_path, "{name}: {body}");
        insta::assert_snapshot!(format!("validated_422_deserialize_{name}"), body);
    }
}

// ── Path / Form rejection kinds → envelope `path` (or pass-through) ──

/// Single-param newtype: a parse failure carries no key (`ParseError`).
#[derive(Deserialize, garde::Validate)]
#[serde(transparent)]
#[allow(dead_code)]
struct Num(#[garde(skip)] u32);

/// Tuple struct: per-segment parse failures are reported by index.
#[derive(Deserialize, garde::Validate)]
#[allow(dead_code)]
struct Pair(#[garde(skip)] u32, #[garde(skip)] u32);

/// Parses fine but rejects odd values with a custom serde message, which axum
/// surfaces as `ErrorKind::Message` (no key).
#[derive(garde::Validate)]
#[allow(dead_code)]
struct Even(#[garde(skip)] u32);

impl<'de> Deserialize<'de> for Even {
    fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = u32::deserialize(deserializer)?;
        if n % 2 == 0 {
            Ok(Self(n))
        } else {
            Err(::serde::de::Error::custom("value must be even"))
        }
    }
}

#[derive(Deserialize, garde::Validate)]
struct Address {
    #[garde(skip)]
    #[allow(dead_code)]
    city: String,
}

#[derive(Deserialize, garde::Validate)]
struct Customer {
    #[garde(dive)]
    #[allow(dead_code)]
    address: Address,
}

async fn num(Validated(Path(_n)): Validated<Path<Num>>) -> &'static str {
    "ok"
}

async fn pair(Validated(Path(_p)): Validated<Path<Pair>>) -> &'static str {
    "ok"
}

async fn even(Validated(Path(_e)): Validated<Path<Even>>) -> &'static str {
    "ok"
}

async fn customer(Validated(::axum::Json(_c)): Validated<::axum::Json<Customer>>) -> &'static str {
    "ok"
}

fn rejection_kind_router() -> Router {
    Router::new()
        .route("/num/{n}", get(num))
        .route("/pair/{a}/{b}", get(pair))
        .route("/even/{n}", get(even))
        // Server-side misuse: two-element tuple on a one-param route, and a
        // `Path` extractor on a route with no params at all.
        .route("/one/{a}", get(pair))
        .route("/no-params", get(num))
        // `Form` on GET reads the query string (`FailedToDeserializeForm`).
        .route("/login", get(login))
        .route("/customers", post(customer))
}

async fn send(req: Request<Body>) -> (u16, String) {
    let res = rejection_kind_router().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    (status, body_to_string(res.into_body()).await)
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[rstest::rstest]
#[case::path_parse_error_without_key("/num/abc", "")]
#[case::path_parse_error_at_index("/pair/1/x", "1")]
#[case::path_custom_message("/even/3", "")]
#[case::form_on_get_reads_query_string("/login?username=alice", "password")]
#[tokio::test]
async fn data_errors_map_to_the_envelope_path(#[case] uri: &str, #[case] expected_path: &str) {
    let (status, body) = send(get_req(uri)).await;
    assert_eq!(status, 422, "{uri}: {body}");
    let parsed: ::serde_json::Value = ::serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["errors"][0]["path"], expected_path, "{uri}: {body}");
    assert!(
        !parsed["errors"][0]["message"].as_str().unwrap().is_empty(),
        "{uri}: {body}"
    );
}

#[tokio::test]
async fn custom_path_message_is_reported_verbatim() {
    let (status, body) = send(get_req("/even/3")).await;
    assert_eq!(status, 422);
    let parsed: ::serde_json::Value = ::serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["errors"][0]["message"], "value must be even");
}

/// Route/extractor mismatches are server bugs: `Validated` must not dress
/// them up as a client `422` — axum's own `500` passes through unchanged.
#[rstest::rstest]
#[case::wrong_number_of_parameters("/one/1")]
#[case::path_extractor_on_route_without_params("/no-params")]
#[tokio::test]
async fn server_side_path_misuse_passes_through_as_500(#[case] uri: &str) {
    let (status, body) = send(get_req(uri)).await;
    assert_eq!(status, 500, "{uri}: {body}");
    assert!(
        ::serde_json::from_str::<::serde_json::Value>(&body).is_err(),
        "{uri} must keep axum's plain-text body, got {body}"
    );
}

/// serde reports `missing field` at the PARENT path; the envelope points at
/// the missing field itself, joined with a `.` below the root.
#[tokio::test]
async fn nested_missing_field_path_points_at_the_field() {
    let req = Request::builder()
        .method("POST")
        .uri("/customers")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"address":{}}"#))
        .unwrap();
    let (status, body) = send(req).await;
    assert_eq!(status, 422, "{body}");
    let parsed: ::serde_json::Value = ::serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["errors"][0]["path"], "address.city", "{body}");
}

#[derive(::vespera::Multipart, garde::Validate)]
#[allow(dead_code)]
struct Upload {
    #[garde(length(min = 1))]
    name: String,
}

async fn upload(
    Validated(::vespera::multipart::TypedMultipart(_u)): Validated<
        ::vespera::multipart::TypedMultipart<Upload>,
    >,
) -> &'static str {
    "ok"
}

async fn upload_unvalidated(
    ::vespera::multipart::TypedMultipart(_u): ::vespera::multipart::TypedMultipart<Upload>,
) -> &'static str {
    "ok"
}

/// Rejections of extractors other than `Json`/`Form`/`Query`/`Path` (here
/// `TypedMultipart` without a multipart content type) are not re-shaped:
/// the inner extractor's own response passes through untouched.
#[tokio::test]
async fn non_serde_extractor_rejection_passes_through_unchanged() {
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/upload")
            .header("content-type", "text/plain")
            .body(Body::from("name=x"))
            .unwrap()
    };
    let validated = Router::new().route("/upload", post(upload));
    let res = validated.oneshot(request()).await.unwrap();
    let (status, body) = (res.status(), body_to_string(res.into_body()).await);
    let unwrapped = Router::new().route("/upload", post(upload_unvalidated));
    let raw_res = unwrapped.oneshot(request()).await.unwrap();
    assert_ne!(status, 200);
    assert_eq!(status, raw_res.status());
    assert_eq!(body, body_to_string(raw_res.into_body()).await);
}
