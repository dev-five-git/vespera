//! Regression test for INP-04: a response body that errors mid-stream
//! must surface as a `500` wire response — never the original status
//! with a silently-truncated (empty) body.
//!
//! Runs in its own test binary because [`register_app`] is a
//! process-global first-wins registration; isolating it keeps this
//! erroring app from leaking into other integration tests.

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use futures_util::stream;
use tokio::runtime::Builder;
use vespera_inprocess::{Router, dispatch_from_bytes, register_app};

/// A `200 OK` whose body's only frame is an error — collecting it fails
/// partway, which the buffered dispatch path must report as a `500`.
async fn erroring_body() -> Response {
    let s =
        stream::once(async { Err::<bytes::Bytes, std::io::Error>(std::io::Error::other("boom")) });
    Response::new(Body::from_stream(s))
}

async fn erroring_422_body() -> Response {
    let stream = stream::iter([
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial")),
        Err(std::io::Error::other("boom")),
    ]);
    Response::builder()
        .status(StatusCode::UNPROCESSABLE_ENTITY)
        .body(Body::from_stream(stream))
        .expect("valid test response")
}

fn assemble_wire(method: &str, path: &str) -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"{method}","path":"{path}"}}"#);
    let mut wire = Vec::new();
    wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(header.as_bytes());
    wire
}

#[test]
fn response_body_stream_error_becomes_500() {
    register_app(|| {
        Router::new()
            .route("/boom", get(erroring_body))
            .route("/boom-422", get(erroring_422_body))
    });

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let resp = dispatch_from_bytes(assemble_wire("GET", "/boom"), &runtime);

    assert!(resp.len() >= 4, "wire response too short");
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON");
    assert_eq!(
        header["status"].as_u64(),
        Some(500),
        "a mid-stream response body error must become a 500, not a silent empty success \
         (the handler's 200 status must NOT be reported with a truncated body)"
    );
}

#[test]
fn response_body_stream_error_during_422_hoisting_becomes_500() {
    register_app(|| {
        Router::new()
            .route("/boom", get(erroring_body))
            .route("/boom-422", get(erroring_422_body))
    });

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let resp = dispatch_from_bytes(assemble_wire("GET", "/boom-422"), &runtime);

    assert!(resp.len() >= 4, "wire response too short");
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON");
    assert_eq!(header["status"].as_u64(), Some(500));
    assert_eq!(
        &resp[4 + header_len..],
        b"response body stream error",
        "a failed 422 collection must emit the canonical body-stream error"
    );
}
