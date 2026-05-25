//! End-to-end integration test for the JNI dispatch path
//! ([`vespera_inprocess::dispatch_from_bytes`]) + the `Validated<T>`
//! extractor.
//!
//! Pins the contract that **422 validation failures crossing the JNI
//! boundary surface as a wire-format response with `status: 422`,
//! `content-type: application/json`, and a body containing the
//! structured error array** — the Java side must be able to read
//! both the status and the error array out of the wire response it
//! receives back from the cdylib.

#![cfg(feature = "validation")]

use ::axum::{Router, routing::post};
use ::serde::Deserialize;
use ::serde_json::{Value, json};
use ::std::collections::HashMap;
use ::std::sync::Once;
use ::tokio::runtime::Builder;
use ::vespera::{Schema, Validated, axum::Json};
use ::vespera_inprocess::{dispatch_from_bytes, register_app};

#[derive(Deserialize, Schema)]
#[allow(dead_code)]
struct JniReq {
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    username: String,

    #[schema(format = "email")]
    email: String,

    #[schema(minimum = 18, maximum = 150)]
    age: u32,
}

async fn jni_handler(
    Validated(Json(_payload)): Validated<Json<JniReq>>,
) -> &'static str {
    "ok"
}

fn jni_router() -> Router {
    Router::new().route("/v/users", post(jni_handler))
}

fn install_router_once() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_app(jni_router));
}

/// Encode a POST wire request with body bytes + headers.
fn encode_wire_post(path: &str, headers: HashMap<&str, &str>, body: &[u8]) -> Vec<u8> {
    let headers_json: serde_json::Map<String, Value> = headers
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect();
    let header = json!({
        "v": 1,
        "method": "POST",
        "path": path,
        "headers": headers_json,
    });
    let header_bytes = serde_json::to_vec(&header).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

/// Decode a wire response: `(header_json, body_bytes)`.
fn decode_wire(resp: &[u8]) -> (Value, Vec<u8>) {
    assert!(resp.len() >= 4, "wire response too short");
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(4 + header_len <= resp.len(), "header_len overflows response");
    let header: Value = serde_json::from_slice(&resp[4..4 + header_len])
        .expect("response header is valid JSON");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

/// Dispatch a JSON body and return `(header, body_as_value)`.  Body is
/// parsed as JSON because validation failures emit JSON.
fn dispatch_json_body(body: &Value) -> (Value, Value) {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let body_bytes = body.to_string().into_bytes();
    let wire = encode_wire_post(
        "/v/users",
        HashMap::from([("content-type", "application/json")]),
        &body_bytes,
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body_bytes) = decode_wire(&resp);
    let body_value: Value = if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or(Value::Null)
    };
    (header, body_value)
}

fn good_body() -> Value {
    json!({
        "username": "alice_99",
        "email":    "alice@example.com",
        "age":      30
    })
}

#[test]
fn jni_dispatch_valid_payload_returns_200_envelope() {
    let (header, body) = dispatch_json_body(&good_body());
    assert_eq!(
        header["status"].as_u64().expect("status is integer"),
        200,
        "valid payload must produce 200: header={header:#} body={body:#}"
    );
    // 200 OK body is the literal string "ok" (axum's IntoResponse for &'static str).
    // It's not JSON, so body_value falls through to Null; verify via raw bytes too.
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let wire = encode_wire_post(
        "/v/users",
        HashMap::from([("content-type", "application/json")]),
        good_body().to_string().as_bytes(),
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (_h, body_bytes) = decode_wire(&resp);
    assert_eq!(
        String::from_utf8_lossy(&body_bytes),
        "ok",
        "valid payload body must be \"ok\""
    );
}

#[test]
fn jni_dispatch_short_username_returns_422_envelope_with_path() {
    let body = json!({
        "username": "x",
        "email": "alice@example.com",
        "age": 30
    });
    let (header, error_body) = dispatch_json_body(&body);

    assert_eq!(
        header["status"].as_u64().expect("status is integer"),
        422,
        "validation failure must surface in wire.status: header={header:#}"
    );

    let content_type = header["headers"]["content-type"]
        .as_str()
        .expect("content-type header missing");
    assert_eq!(content_type, "application/json");

    // (a) Original body preserved (BC contract): errors array still in body.
    let errors = error_body["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("errors array missing in body: {error_body:#}"));
    assert!(
        errors.iter().any(|e| e["path"].as_str() == Some("username")),
        "expected `username` in error paths, got {error_body:#}"
    );

    // (b) NEW: validation_errors hoisted into wire header — single parse for Java.
    let hoisted = header["validation_errors"]
        .as_array()
        .unwrap_or_else(|| panic!("validation_errors missing in wire header: {header:#}"));
    assert!(
        hoisted
            .iter()
            .any(|e| e["path"].as_str() == Some("username")),
        "expected `username` in hoisted validation_errors, got {header:#}"
    );
}

#[test]
fn jni_dispatch_invalid_email_returns_422_envelope() {
    let body = json!({
        "username": "valid_user",
        "email": "not-an-email",
        "age": 30
    });
    let (header, error_body) = dispatch_json_body(&body);

    assert_eq!(header["status"].as_u64().unwrap(), 422);
    let errors = error_body["errors"].as_array().unwrap();
    assert!(errors.iter().any(|e| e["path"].as_str() == Some("email")));
}

#[test]
fn jni_dispatch_out_of_range_age_returns_422_envelope() {
    let body = json!({
        "username": "valid_user",
        "email": "alice@example.com",
        "age": 200_u32
    });
    let (header, error_body) = dispatch_json_body(&body);

    assert_eq!(header["status"].as_u64().unwrap(), 422);
    let errors = error_body["errors"].as_array().unwrap();
    assert!(errors.iter().any(|e| e["path"].as_str() == Some("age")));
}

#[test]
fn jni_dispatch_multiple_violations_envelope_contains_all_paths() {
    let body = json!({
        "username": "X",         // pattern + min_length
        "email":    "broken",    // format=email
        "age":      9999_u32     // range max
    });
    let (header, error_body) = dispatch_json_body(&body);

    assert_eq!(header["status"].as_u64().unwrap(), 422);
    let errors = error_body["errors"].as_array().unwrap();
    let paths: Vec<&str> = errors
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(paths.contains(&"username"), "got {paths:?}");
    assert!(paths.contains(&"email"), "got {paths:?}");
    assert!(paths.contains(&"age"), "got {paths:?}");

    // NEW: hoisted validation_errors must mirror the body.
    let hoisted = header["validation_errors"].as_array().unwrap();
    let hoisted_paths: Vec<&str> = hoisted
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(hoisted_paths.contains(&"username"), "got {hoisted_paths:?}");
    assert!(hoisted_paths.contains(&"email"), "got {hoisted_paths:?}");
    assert!(hoisted_paths.contains(&"age"), "got {hoisted_paths:?}");
}

#[test]
fn jni_dispatch_200_response_does_not_carry_validation_errors() {
    let (header, _body) = dispatch_json_body(&good_body());
    assert_eq!(header["status"].as_u64().unwrap(), 200);
    assert!(
        header["validation_errors"].is_null(),
        "200 response must not carry validation_errors field, got {header:#}"
    );
}
