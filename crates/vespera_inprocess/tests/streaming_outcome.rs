//! Header-LAST response streaming (`dispatch_streaming_async` /
//! `dispatch_streaming_with_outcome_async`) parity with the other modes:
//!
//! * 422 `validation_errors` are hoisted into the returned header;
//! * an oversized 422 body (beyond the 64 KiB hoist bound) is forwarded
//!   verbatim and unhoisted on every mode instead of being collected;
//! * a body failure AFTER chunks were delivered is reported as a truncation
//!   (`StreamOutcome::BodyError` / `SinkStopped`), while a failure before
//!   the first chunk is a clean, complete error response.
//!
//! Runs with the default (unset) limits and timeout.

use std::ops::ControlFlow;

use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;
use rstest::rstest;
use serde_json::{Value, json};
use vespera_inprocess::{
    DirectWriteResult, StreamOutcome, dispatch_from_bytes_async, dispatch_into_async,
    dispatch_streaming_async, dispatch_streaming_with_header_async,
    dispatch_streaming_with_outcome_async, register_app_named,
};

const APP: &str = "streaming_outcome";
const VALIDATION_BODY: &str = r#"{"errors":[{"path":"username","message":"too short"}]}"#;
/// Larger than the 64 KiB hoist bound.
const BIG_422_LEN: usize = 100 * 1024;

async fn validate_fail() -> axum::response::Response {
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        VALIDATION_BODY,
    )
        .into_response()
}

/// A 422 whose JSON body is far beyond the hoist bound, in two frames.
async fn big_validate_fail() -> axum::response::Response {
    let mut first = br#"{"errors":[{"path":"a","message":"b"}],"pad":""#.to_vec();
    first.resize(BIG_422_LEN / 2, b'x');
    let mut second = vec![b'y'; BIG_422_LEN / 2];
    second.extend_from_slice(br#""}"#);
    let stream = futures_util::stream::iter([
        Ok::<_, std::convert::Infallible>(Bytes::from(first)),
        Ok(Bytes::from(second)),
    ]);
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Body::from_stream(stream),
    )
        .into_response()
}

/// Two good frames, then a body error.
async fn fails_after_chunks() -> Body {
    let stream = futures_util::stream::iter([
        Ok(Bytes::from_static(b"first")),
        Ok(Bytes::from_static(b"second")),
        Err(std::io::Error::other("boom")),
    ]);
    Body::from_stream(stream)
}

/// A body error before any data frame.
async fn fails_immediately() -> Body {
    let stream = futures_util::stream::iter([Err::<Bytes, _>(std::io::Error::other("boom"))]);
    Body::from_stream(stream)
}

fn install() {
    register_app_named(APP, || {
        Router::new()
            .route("/validate", post(validate_fail))
            .route("/big422", get(big_validate_fail))
            .route("/fails_after_chunks", get(fails_after_chunks))
            .route("/fails_immediately", get(fails_immediately))
    });
}

fn encode(method: &str, path: &str) -> Vec<u8> {
    let header = json!({ "v": 1, "method": method, "path": path, "app": APP,
                         "headers": { "content-type": "application/json" } });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let body: &[u8] = if method == "POST" { br#"{"x":1}"# } else { b"" };
    let mut wire = u32::try_from(header_bytes.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

fn split(wire: &[u8]) -> (Value, &[u8]) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    (
        serde_json::from_slice(&wire[4..4 + len]).unwrap(),
        &wire[4 + len..],
    )
}

#[tokio::test]
async fn header_last_streaming_hoists_422_validation_errors() {
    install();
    let mut body = Vec::new();
    let header = dispatch_streaming_async(encode("POST", "/validate"), |c: &[u8]| {
        body.extend_from_slice(c);
        ControlFlow::Continue(())
    })
    .await;
    let (json, tail) = split(&header);
    assert_eq!(json["status"], 422);
    assert_eq!(json["validation_errors"][0]["path"], "username");
    assert!(tail.is_empty(), "header-only return");
    assert_eq!(
        body,
        VALIDATION_BODY.as_bytes(),
        "body still streamed verbatim"
    );
}

#[tokio::test]
async fn oversized_422_is_forwarded_unhoisted_on_buffered_and_direct_paths() {
    install();
    let wire = dispatch_from_bytes_async(encode("GET", "/big422")).await;
    let (json, body) = split(&wire);
    assert_eq!(json["status"], 422);
    assert!(json.get("validation_errors").is_none(), "{json}");
    assert_eq!(body.len(), BIG_422_LEN + 2);

    let mut out = vec![0u8; BIG_422_LEN * 2];
    let DirectWriteResult::Complete(n) =
        dispatch_into_async(encode("GET", "/big422"), &mut out).await
    else {
        panic!("direct write must fit");
    };
    assert_eq!(
        &out[..n],
        &wire[..],
        "direct and buffered agree byte-for-byte"
    );
}

#[tokio::test]
async fn oversized_422_is_streamed_unhoisted_header_first() {
    install();
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_streaming_with_header_async(
        encode("GET", "/big422"),
        |h: &[u8]| header.extend_from_slice(h),
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    let (json, _) = split(&header);
    assert_eq!(json["status"], 422);
    assert!(json.get("validation_errors").is_none());
    assert_eq!(body.len(), BIG_422_LEN + 2, "prefix + rest delivered");
}

#[rstest]
#[case::error_after_chunks("/fails_after_chunks", StreamOutcome::BodyError, true)]
#[case::error_before_first_chunk("/fails_immediately", StreamOutcome::Complete, false)]
#[tokio::test]
async fn outcome_distinguishes_truncation_from_clean_error(
    #[case] path: &str,
    #[case] expected: StreamOutcome,
    #[case] expect_chunks: bool,
) {
    install();
    let mut chunks = 0;
    let (header, outcome) =
        dispatch_streaming_with_outcome_async(encode("GET", path), |_c: &[u8]| {
            chunks += 1;
            ControlFlow::Continue(())
        })
        .await;
    assert_eq!(outcome, expected);
    assert_eq!(chunks > 0, expect_chunks);
    let (json, _) = split(&header);
    assert_eq!(json["status"], 500, "both report a 5xx header");
}

#[tokio::test]
async fn sink_stop_after_first_chunk_is_reported_as_truncation() {
    install();
    let (_header, outcome) = dispatch_streaming_with_outcome_async(
        encode("GET", "/fails_after_chunks"),
        |_c: &[u8]| ControlFlow::Break(()),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(outcome.is_truncated());
}
