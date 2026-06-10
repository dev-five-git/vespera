//! Echo routes — used by the manual QA / curl smoke tests for the
//! streaming JNI surfaces.  The `/echo` handler returns the request
//! body verbatim with the same `Content-Type`; pair it with
//! `dispatchFullStreamingWithHeader` from the Java side to verify
//! byte-for-byte 1 GiB ↔ 1 GiB round-trips.

use vespera::axum::body::Bytes;
use vespera::axum::http::HeaderMap;
use vespera::axum::http::header;
use vespera::axum::response::{IntoResponse, Response};

/// Echo the request body back as the response body verbatim,
/// preserving the incoming `Content-Type`.  Mounted at `/echo`
/// (path derived from the source file name, matching the convention
/// used by `health.rs`).
#[allow(clippy::unused_async)]
#[vespera::route(post, tags = ["echo"])]
pub async fn echo(headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

/// **Streaming** echo — passes the request body stream straight
/// through as the response body without ever buffering it.  Unlike
/// `/echo` (which extracts `Bytes` and is therefore subject to axum's
/// 2 MiB `DefaultBodyLimit`), this handler consumes the raw
/// [`vespera::axum::body::Body`], so multi-GiB bidirectional streams
/// can be exercised end-to-end — used by the JNI streaming throughput
/// benchmark (`StreamingThroughputBenchTest`).
#[allow(clippy::unused_async)]
#[vespera::route(post, path = "/stream", tags = ["echo"])]
pub async fn echo_stream(body: vespera::axum::body::Body) -> Response {
    Response::new(body)
}
