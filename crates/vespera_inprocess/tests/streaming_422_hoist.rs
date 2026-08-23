//! The header-first streaming variants hoist 422 `validation_errors` into the
//! wire header (parity with the buffered / direct dispatch paths), while every
//! non-422 streaming response keeps its hoist-free header + streamed body.

use std::ops::ControlFlow;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use bytes::Bytes;
use serde_json::json;
use vespera_inprocess::{
    RequestChunk, StreamOutcome, dispatch_bidirectional_streaming_with_header,
    dispatch_streaming_with_header_async, register_app,
};

const VALIDATION_BODY: &str = r#"{"errors":[{"path":"username","message":"length is lower than 3"},{"path":"email","message":"not a valid email"}]}"#;

async fn validate_fail() -> axum::response::Response {
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        VALIDATION_BODY,
    )
        .into_response()
}

async fn echo(body: Bytes) -> Bytes {
    body
}

fn install() {
    register_app(|| {
        Router::new()
            .route("/validate", post(validate_fail))
            .route("/echo", post(echo))
    });
}

/// Assemble `[u32 BE header_len | header JSON | body]` wire bytes.
fn encode(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let header_map: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
        .collect();
    let header = json!({ "v": 1, "method": method, "path": path, "headers": header_map });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

/// Decode captured `[u32 BE header_len | JSON]` header bytes into the JSON text.
fn header_json(header_bytes: &[u8]) -> String {
    let len = u32::from_be_bytes(header_bytes[0..4].try_into().unwrap()) as usize;
    String::from_utf8(header_bytes[4..4 + len].to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_streaming_with_header_hoists_422() {
    install();
    let wire = encode(
        "POST",
        "/validate",
        &[("content-type", "application/json")],
        br#"{"x":1}"#,
    );
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_streaming_with_header_async(
        wire,
        |h: &[u8]| header.extend_from_slice(h),
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);

    let json = header_json(&header);
    assert!(
        json.contains("\"validation_errors\""),
        "422 streaming header must hoist validation_errors: {json}"
    );
    assert!(
        json.contains("username") && json.contains("email"),
        "hoisted paths must appear in the header: {json}"
    );
    // The original body is still delivered verbatim through on_chunk.
    assert_eq!(String::from_utf8(body).unwrap(), VALIDATION_BODY);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_streaming_with_header_non_422_has_no_hoist() {
    install();
    let wire = encode(
        "POST",
        "/echo",
        &[("content-type", "application/json")],
        br#"{"hello":"world"}"#,
    );
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_streaming_with_header_async(
        wire,
        |h: &[u8]| header.extend_from_slice(h),
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);

    let json = header_json(&header);
    assert!(
        !json.contains("validation_errors"),
        "non-422 header must NOT hoist validation_errors: {json}"
    );
    assert!(
        json.contains("\"status\":200"),
        "expected 200 status: {json}"
    );
    assert_eq!(String::from_utf8(body).unwrap(), r#"{"hello":"world"}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_with_header_hoists_422() {
    install();
    // Bidirectional input is header-only; the body arrives via pull_chunk. The
    // /validate handler returns 422 without reading the body, so End suffices.
    let wire = encode(
        "POST",
        "/validate",
        &[("content-type", "application/json")],
        b"",
    );
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_bidirectional_streaming_with_header(
        wire,
        || RequestChunk::End,
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);

    let json = header_json(&header);
    assert!(
        json.contains("\"validation_errors\""),
        "bidirectional 422 header must hoist validation_errors: {json}"
    );
    assert_eq!(String::from_utf8(body).unwrap(), VALIDATION_BODY);
}
