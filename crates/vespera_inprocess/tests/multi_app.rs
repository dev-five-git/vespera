//! Integration tests for multi-app routing
//! ([`vespera_inprocess::register_app_named`]).
//!
//! Validates that multiple named axum routers can coexist in the
//! same process and dispatch correctly based on the wire header's
//! `"app"` field.

use std::collections::HashMap;
use std::sync::Once;

use axum::Router;
use axum::routing::get;
use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::{dispatch_from_bytes, register_app, register_app_named};

fn admin_router() -> Router {
    Router::new()
        .route("/dashboard", get(|| async { "admin-dashboard" }))
        .route("/users", get(|| async { "admin-users" }))
}

fn public_router() -> Router {
    Router::new()
        .route("/health", get(|| async { "public-health" }))
        .route("/about", get(|| async { "public-about" }))
}

fn default_router() -> Router {
    Router::new().route("/root", get(|| async { "default-root" }))
}

/// Register all three apps exactly once per test binary.
fn install_all_apps() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_app(default_router);
        register_app_named("admin", admin_router);
        register_app_named("public", public_router);
    });
}

fn encode_wire(method: &str, path: &str, app: Option<&str>) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    if let Some(a) = app {
        header.insert("app".to_owned(), Value::String(a.to_owned()));
    }
    let header_bytes =
        serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire
}

fn dispatch(wire: Vec<u8>) -> (Value, Vec<u8>) {
    install_all_apps();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let resp = dispatch_from_bytes(wire, &runtime);
    assert!(resp.len() >= 4);
    let len_bytes: [u8; 4] = resp[..4].try_into().unwrap();
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    let header: Value = serde_json::from_slice(&resp[4..4 + header_len]).unwrap();
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

#[test]
fn default_app_reachable_when_app_omitted() {
    let (header, body) = dispatch(encode_wire("GET", "/root", None));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"default-root");
}

#[test]
fn default_app_reachable_with_explicit_default_name() {
    let (header, body) = dispatch(encode_wire("GET", "/root", Some("_default")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"default-root");
}

#[test]
fn empty_app_name_falls_back_to_default() {
    // Empty string should be treated the same as omission, not as an
    // invalid name (matches Oracle Q7 recommendation).
    let (header, body) = dispatch(encode_wire("GET", "/root", Some("")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"default-root");
}

#[test]
fn admin_app_routes_isolated_from_public_app() {
    // /dashboard exists only on the admin app
    let (header, body) = dispatch(encode_wire("GET", "/dashboard", Some("admin")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"admin-dashboard");

    // /dashboard does NOT exist on public — 404 from axum
    let (header, _) = dispatch(encode_wire("GET", "/dashboard", Some("public")));
    assert_eq!(header["status"].as_u64(), Some(404));
}

#[test]
fn public_app_routes_isolated_from_admin_app() {
    let (header, body) = dispatch(encode_wire("GET", "/health", Some("public")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"public-health");

    // Admin doesn't have /health
    let (header, _) = dispatch(encode_wire("GET", "/health", Some("admin")));
    assert_eq!(header["status"].as_u64(), Some(404));
}

#[test]
fn default_app_does_not_see_named_app_routes() {
    // /dashboard exists on admin, but the default app doesn't have it
    let (header, _) = dispatch(encode_wire("GET", "/dashboard", None));
    assert_eq!(header["status"].as_u64(), Some(404));
}

#[test]
fn unknown_app_name_returns_404() {
    let (header, body) = dispatch(encode_wire("GET", "/anything", Some("nonexistent")));
    assert_eq!(header["status"].as_u64(), Some(404));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("no app registered with name 'nonexistent'"),
        "explanation should name the missing app, got {msg}"
    );
}

#[test]
fn invalid_app_name_with_special_chars_returns_400() {
    let (header, body) = dispatch(encode_wire("GET", "/root", Some("bad name!")));
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("invalid app name"),
        "expected 'invalid app name' explanation, got {msg}"
    );
}

#[test]
fn whitespace_only_app_name_falls_back_to_default() {
    // "   " trims to empty → treated as default per resolve_app_router.
    let (header, body) = dispatch(encode_wire("GET", "/root", Some("   ")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"default-root");
}

#[test]
fn register_app_named_first_wins_per_name() {
    // After install_all_apps, "admin" router has /dashboard.  A second
    // register_app_named call with the same name must NOT replace the
    // first registration.
    install_all_apps();
    register_app_named("admin", || {
        Router::new().route("/intruder", get(|| async { "should-not-be-reachable" }))
    });

    // Original /dashboard route still works
    let (header, body) = dispatch(encode_wire("GET", "/dashboard", Some("admin")));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"admin-dashboard");

    // The second-registration route is NOT reachable
    let (header, _) = dispatch(encode_wire("GET", "/intruder", Some("admin")));
    assert_eq!(header["status"].as_u64(), Some(404));
}

#[test]
fn register_app_named_with_invalid_name_is_silently_discarded() {
    register_app_named("bad name!", || {
        Router::new().route("/whatever", get(|| async { "should-not-register" }))
    });
    // The invalid registration should not have created an app,
    // so dispatching to "bad name!" still returns 400 (invalid name),
    // not 200.
    let (header, _) = dispatch(encode_wire("GET", "/whatever", Some("bad name!")));
    assert_eq!(header["status"].as_u64(), Some(400));
}

#[test]
fn headers_forwarded_to_correct_app() {
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String("GET".to_owned()));
    header.insert("path".to_owned(), Value::String("/users".to_owned()));
    header.insert("app".to_owned(), Value::String("admin".to_owned()));
    let mut headers_obj = serde_json::Map::new();
    headers_obj.insert("x-custom".to_owned(), Value::String("hello".to_owned()));
    header.insert("headers".to_owned(), Value::Object(headers_obj));
    let header_bytes = serde_json::to_vec(&Value::Object(header)).unwrap();
    let header_len = u32::try_from(header_bytes.len()).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);

    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"admin-users");
    // Just confirm dispatch succeeded; the test handler doesn't echo
    // headers — the multi_app integration concern is only that the
    // request reached the correct app router.
    let _ = HashMap::<String, String>::new();
}
