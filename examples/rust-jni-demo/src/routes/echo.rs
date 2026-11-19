//! Echo routes — used by the manual QA / curl smoke tests for the
//! streaming JNI surfaces.  The `/echo` handler returns the request
//! body verbatim with the request's `Content-Type` when it is allowlisted
//! (buffered, so capped by
//! axum's 2 MiB `DefaultBodyLimit`); for byte-for-byte 1 GiB ↔ 1 GiB
//! round-trips pair `/echo/stream` with `dispatchFullStreamingWithHeader`
//! from the Java side.

use vespera::axum::body::Bytes;
use vespera::axum::http::HeaderMap;
use vespera::axum::http::header;
use vespera::axum::response::{IntoResponse, Response};

/// The media types `/echo` reflects.  Any other request `Content-Type`
/// (`text/html`, `image/svg+xml`, ...) is answered as
/// `application/octet-stream`, so the demo never serves attacker-chosen
/// markup under a type a browser would render.
const REFLECTED_MEDIA_TYPES: [&str; 3] =
    ["application/json", "text/plain", "application/octet-stream"];

/// The `Content-Type` `/echo` answers with for the request's `raw` one: the
/// raw value (parameters included) when its media type is allowlisted,
/// otherwise `application/octet-stream`.
fn reflected_content_type(raw: Option<&str>) -> &str {
    match raw {
        Some(value)
            if REFLECTED_MEDIA_TYPES.contains(
                &value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
            ) =>
        {
            value
        }
        _ => "application/octet-stream",
    }
}

/// Echo the request body back as the response body verbatim, with the
/// request's `Content-Type` when it is an allowlisted media type (see
/// [`reflected_content_type`]) and `X-Content-Type-Options: nosniff`.
/// Mounted at `/echo` (path derived from the source file name, matching the
/// convention used by `health.rs`).
#[allow(clippy::unused_async)]
#[vespera::route(post, tags = ["echo"])]
pub async fn echo(headers: HeaderMap, body: Bytes) -> Response {
    let ct = reflected_content_type(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    );
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
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

/// Always panics — exercises the JNI "header callback exactly once"
/// contract from the Java side.  When this handler panics before
/// producing status/headers, `dispatchStreamingWithHeader` /
/// `dispatchFullStreamingWithHeader` must still invoke the header
/// consumer once with a wire-format `500` header (the `header_sent`
/// fallback) rather than leaving the caller hanging.  Used by
/// `StreamingClosureStressTest`'s panic-fallback e2e case.
#[allow(clippy::unused_async, clippy::panic)]
#[vespera::route(post, path = "/panic", tags = ["echo"])]
pub async fn echo_panic() -> Response {
    panic!("intentional handler panic for the header-once fallback e2e test");
}

#[cfg(test)]
mod tests {
    use vespera::axum::http::HeaderValue;

    use super::*;

    #[test]
    fn reflects_only_allowlisted_media_types() {
        let cases = [
            (Some("application/json"), "application/json"),
            (
                Some("application/json; charset=utf-8"),
                "application/json; charset=utf-8",
            ),
            (Some("Text/Plain"), "Text/Plain"),
            (Some("application/octet-stream"), "application/octet-stream"),
            (Some("text/html"), "application/octet-stream"),
            (Some("image/svg+xml"), "application/octet-stream"),
            (None, "application/octet-stream"),
        ];
        for (raw, expected) in cases {
            assert_eq!(reflected_content_type(raw), expected, "raw = {raw:?}");
        }
    }

    #[tokio::test]
    async fn echo_answers_html_as_octet_stream_with_nosniff() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        let response = echo(headers, Bytes::from_static(b"<script>alert(1)</script>")).await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/octet-stream"))
        );
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
    }
}
