//! Coverage-focused tests for the public surface that the larger
//! integration suites don't reach:
//!
//! - the text-envelope [`dispatch`] variant
//! - [`error_envelope`] helper
//! - [`register_app_named`] with names that fail validation
//! - the regular streaming `_async` (no header callback) error
//!   variants (version mismatch / unknown app / invalid HTTP method)
//! - body-without-content-type defaulting to `application/json`
//! - triple-occurrence headers exercising the `Multi → Multi` growth
//!   branch inside `collect_response_parts` and
//!   `dispatch_response_streaming`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use axum::Router;
use axum::http::HeaderMap;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::{
    RequestEnvelope, dispatch, dispatch_from_bytes, dispatch_streaming_async, error_envelope,
    register_app_named,
};

// ── Test app installed under a unique name ──────────────────────────

async fn ping() -> &'static str {
    "pong"
}

/// Echoes the request's content-type header back as the response body
/// so the test can confirm the default-application/json fallback path.
async fn echo_ct(headers: HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("MISSING")
        .to_owned()
}

async fn triple_header() -> Response {
    use axum::http::{HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    let name = HeaderName::from_static("x-trace-id");
    headers.append(name.clone(), HeaderValue::from_static("a"));
    headers.append(name.clone(), HeaderValue::from_static("b"));
    headers.append(name, HeaderValue::from_static("c"));
    (headers, "trace").into_response()
}

/// Echoes back the raw query string the handler observes; used to
/// confirm the `format!("{path}?{query}")` reassembly inside
/// `dispatch_response_streaming`.
async fn echo_query(uri: axum::http::Uri) -> String {
    uri.query().unwrap_or("").to_owned()
}

fn make_router() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/echo_ct", post(echo_ct))
        .route("/triple", get(triple_header))
        .route("/q", get(echo_query))
}

const APP: &str = "misc_coverage_app";

fn install_router() {
    static INIT: Once = Once::new();
    INIT.call_once(|| register_app_named(APP, make_router));
}

fn rt() -> tokio::runtime::Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn encode_wire(
    version: u8,
    method: &str,
    path: &str,
    headers: HashMap<&str, &str>,
    body: &[u8],
    app: Option<&str>,
) -> Vec<u8> {
    let headers_json: serde_json::Map<String, Value> = headers
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect();
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(version));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    if let Some(a) = app {
        header.insert("app".to_owned(), Value::String(a.to_owned()));
    }
    if !headers_json.is_empty() {
        header.insert("headers".to_owned(), Value::Object(headers_json));
    }
    let header_bytes = serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

fn decode_wire(resp: &[u8]) -> (Value, Vec<u8>) {
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    let header: Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("header JSON parses");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

// ── dispatch() (text envelope) ───────────────────────────────────────

#[test]
fn dispatch_text_envelope_returns_serialised_json() {
    install_router();
    let envelope = RequestEnvelope {
        method: "GET".into(),
        path: "/ping".into(),
        ..Default::default()
    };
    let json = rt().block_on(dispatch(make_router(), &envelope));
    let parsed: Value = serde_json::from_str(&json).expect("envelope JSON");
    assert_eq!(parsed["status"].as_u64(), Some(200));
    assert_eq!(parsed["body"].as_str(), Some("pong"));
    assert!(
        parsed["metadata"]["version"].is_string(),
        "metadata.version should always be present"
    );
}

// ── error_envelope() ─────────────────────────────────────────────────

#[test]
fn error_envelope_carries_500_status_and_message() {
    let env = error_envelope("boom!");
    assert_eq!(env.status, 500);
    assert_eq!(env.body, "boom!");
    assert!(env.headers.is_empty(), "no headers on error envelope");
    assert!(!env.metadata.version.is_empty());
}

// ── register_app_named with invalid names is silently discarded ──────

#[test]
fn register_app_named_with_empty_name_is_no_op() {
    register_app_named("", || Router::new().route("/x", get(|| async { "x" })));
    // We can't directly probe absence of "" via dispatch_from_bytes
    // because an empty app field falls back to the default app — but
    // calling the function with empty must be safe (no panic, no
    // state mutation).  The follow-on test confirms the writer path
    // accepted the no-op.
}

#[test]
fn register_app_named_with_whitespace_only_name_is_no_op() {
    register_app_named("   ", || Router::new().route("/x", get(|| async { "x" })));
}

#[test]
fn register_app_named_with_overlong_name_is_no_op() {
    let too_long: String = "a".repeat(128); // > MAX_APP_NAME_LEN (64)
    register_app_named(&too_long, || {
        Router::new().route("/x", get(|| async { "x" }))
    });

    let runtime = rt();
    let wire = encode_wire(1, "GET", "/x", HashMap::new(), &[], Some(&too_long));
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    // Dispatch sees the same too-long name and rejects it at the
    // validate_app_name step with a 400 — proving registration was a
    // no-op (otherwise we'd have hit the app router → 200/404).
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("invalid app name") && msg.contains("too long"),
        "expected 'too long' explanation, got {msg}"
    );
}

// ── dispatch_streaming_async error paths (no header callback) ────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_version_mismatch_returns_400_in_returned_bytes() {
    install_router();
    let wire = encode_wire(99, "GET", "/ping", HashMap::new(), &[], Some(APP));
    let chunks_buf: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = Arc::clone(&chunks_buf);
    let header_bytes =
        dispatch_streaming_async(wire, move |chunk| c.lock().unwrap().push(chunk.to_vec())).await;
    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(msg.contains("unsupported wire version"));
    assert!(chunks_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_unknown_app_returns_404() {
    install_router();
    let wire = encode_wire(
        1,
        "GET",
        "/ping",
        HashMap::new(),
        &[],
        Some("no-such-app-streaming"),
    );
    let chunks_buf: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = Arc::clone(&chunks_buf);
    let header_bytes =
        dispatch_streaming_async(wire, move |chunk| c.lock().unwrap().push(chunk.to_vec())).await;
    let (header, _) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(404));
    assert!(chunks_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_invalid_method_returns_405() {
    install_router();
    let wire = encode_wire(1, "BAD METHOD", "/ping", HashMap::new(), &[], Some(APP));
    let chunks_buf: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = Arc::clone(&chunks_buf);
    let header_bytes =
        dispatch_streaming_async(wire, move |chunk| c.lock().unwrap().push(chunk.to_vec())).await;
    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(405));
    assert!(String::from_utf8_lossy(&body).contains("Method Not Allowed"));
    assert!(chunks_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_triple_header_exercises_multi_growth() {
    install_router();
    let wire = encode_wire(1, "GET", "/triple", HashMap::new(), &[], Some(APP));
    let chunks_buf: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = Arc::clone(&chunks_buf);
    let header_bytes =
        dispatch_streaming_async(wire, move |chunk| c.lock().unwrap().push(chunk.to_vec())).await;
    let (header, _) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));
    let trace = &header["headers"]["x-trace-id"];
    let arr = trace
        .as_array()
        .unwrap_or_else(|| panic!("expected multi-header array, got {trace:#}"));
    let values: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(values, vec!["a", "b", "c"]);
}

// ── body without content-type defaults to application/json ──────────

#[test]
fn body_without_content_type_defaults_to_json() {
    install_router();
    let runtime = rt();
    // POST a non-empty body but DO NOT set content-type — vespera's
    // dispatch_parts must inject application/json so the handler sees
    // a known content-type.
    let wire = encode_wire(1, "POST", "/echo_ct", HashMap::new(), b"{}", Some(APP));
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(
        String::from_utf8_lossy(&body),
        "application/json",
        "missing content-type must default to application/json"
    );
}

// ── streaming with non-empty query / no content-type ──────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_forwards_non_empty_query_string() {
    install_router();
    // encode_wire doesn't carry a query field — hand-roll the header.
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String("GET".into()));
    header.insert("path".to_owned(), Value::String("/q".into()));
    header.insert("query".to_owned(), Value::String("foo=1&bar=baz".into()));
    header.insert("app".to_owned(), Value::String(APP.into()));
    let hb = serde_json::to_vec(&Value::Object(header)).unwrap();
    let hlen = u32::try_from(hb.len()).unwrap();
    let mut wire = Vec::with_capacity(4 + hb.len());
    wire.extend_from_slice(&hlen.to_be_bytes());
    wire.extend_from_slice(&hb);

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&buf);
    let header_bytes = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
    })
    .await;
    let (header_json, _) = decode_wire(&header_bytes);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(
        String::from_utf8(buf.lock().unwrap().clone()).unwrap(),
        "foo=1&bar=baz"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_async_post_body_without_content_type_defaults_to_json() {
    install_router();
    // Same wire shape as misc test but routed to streaming variant.
    let wire = encode_wire(1, "POST", "/echo_ct", HashMap::new(), b"{}", Some(APP));
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&buf);
    let header_bytes = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
    })
    .await;
    let (header_json, _) = decode_wire(&header_bytes);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(
        String::from_utf8(buf.lock().unwrap().clone()).unwrap(),
        "application/json"
    );
}

#[test]
fn collect_response_parts_triple_header_via_text_envelope() {
    // The text-envelope `dispatch_typed` path also runs
    // collect_response_parts; this drives the Multi → Multi branch
    // there in addition to the streaming path above.
    install_router();
    let runtime = rt();
    let envelope = RequestEnvelope {
        method: "GET".into(),
        path: "/triple".into(),
        ..Default::default()
    };
    let env = runtime.block_on(vespera_inprocess::dispatch_typed(make_router(), &envelope));
    let trace = env
        .headers
        .get("x-trace-id")
        .expect("triple header present in envelope");
    match trace {
        vespera_inprocess::HeaderValue::Multi(v) => {
            assert_eq!(v, &vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        }
        vespera_inprocess::HeaderValue::Single(s) => {
            panic!("expected Multi, got Single({s:?})")
        }
    }
}
