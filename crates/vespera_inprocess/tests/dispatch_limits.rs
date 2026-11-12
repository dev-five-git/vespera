//! Process-global resource limits: per-dispatch deadline
//! (`set_dispatch_timeout`), buffered-response cap
//! (`set_max_buffered_response_bytes`) and the bidirectional streaming
//! request cap (`set_max_streaming_request_bytes`).
//!
//! Own test binary: the knobs are first-wins `OnceLock`s, so every test
//! pins the SAME values through [`ensure`] and stays order-independent.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::routing::{get, post};
use bytes::Bytes;
use rstest::rstest;
use serde_json::{Value, json};
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, RequestEnvelope, StreamOutcome,
    dispatch_bidirectional_streaming_with_header, dispatch_from_bytes_async, dispatch_into_async,
    dispatch_owned, dispatch_streaming_with_header_async, dispatch_streaming_with_outcome_async,
    register_app_named, set_dispatch_timeout, set_max_buffered_response_bytes,
    set_max_streaming_request_bytes,
};

const APP: &str = "dispatch_limits";
const TIMEOUT: Duration = Duration::from_millis(200);
const MAX_RESPONSE: usize = 4096;
const MAX_STREAM_REQUEST: usize = 1000;

fn ensure() {
    let _ = set_dispatch_timeout(TIMEOUT);
    let _ = set_max_buffered_response_bytes(MAX_RESPONSE);
    let _ = set_max_streaming_request_bytes(MAX_STREAM_REQUEST);
    register_app_named(APP, || {
        Router::new()
            .route("/slow", get(slow))
            .route("/slow_body", get(slow_body))
            .route("/big_full", get(big_full))
            .route("/big_stream", get(big_stream))
            .route("/small", get(|| async { "ok" }))
            .route("/big_422", get(big_422))
            .route("/small_422", get(small_422))
            .route("/upload", post(upload))
    });
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(30)).await;
    "late"
}

/// Commits a 200 + first chunk immediately, then delivers the rest of the
/// body only after more than twice the dispatch deadline has elapsed.
async fn slow_body() -> Body {
    let stream = futures_util::stream::unfold(0u8, |state| async move {
        match state {
            0 => Some((
                Ok::<_, std::convert::Infallible>(Bytes::from_static(b"head")),
                1,
            )),
            1 => {
                tokio::time::sleep(TIMEOUT * 2).await;
                Some((Ok(Bytes::from_static(b"tail")), 2))
            }
            _ => None,
        }
    });
    Body::from_stream(stream)
}

/// A `422` whose body is over the response cap but under the 64 KiB hoist
/// prefetch bound (so both buffered and direct take the `Small` branch).
async fn big_422() -> (http::StatusCode, String) {
    let filler = "e".repeat(MAX_RESPONSE + 1);
    (
        http::StatusCode::UNPROCESSABLE_ENTITY,
        format!(r#"{{"errors":[{{"path":"x","message":"{filler}"}}]}}"#),
    )
}

async fn small_422() -> (http::StatusCode, &'static str) {
    (
        http::StatusCode::UNPROCESSABLE_ENTITY,
        r#"{"errors":[{"path":"x","message":"bad"}]}"#,
    )
}

async fn big_full() -> Bytes {
    Bytes::from(vec![b'x'; MAX_RESPONSE + 1])
}

/// Unknown-length (no exact size hint) body over the cap.
async fn big_stream() -> Body {
    let frames = (0..3).map(|_| Ok::<_, std::convert::Infallible>(Bytes::from(vec![b'y'; 2048])));
    Body::from_stream(futures_util::stream::iter(frames))
}

async fn upload(body: Bytes) -> String {
    body.len().to_string()
}

fn encode(method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let header = json!({ "v": 1, "method": method, "path": path, "app": APP });
    let header_bytes = serde_json::to_vec(&header).unwrap();
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

fn assert_quick(started: Instant) {
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "dispatch must be bounded by the deadline, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn buffered_dispatch_times_out_with_504() {
    ensure();
    let started = Instant::now();
    let wire = dispatch_from_bytes_async(encode("GET", "/slow", b"")).await;
    assert_quick(started);
    let (json, body) = split(&wire);
    assert_eq!(json["status"], 504);
    assert!(String::from_utf8_lossy(body).contains("VESPERA_DISPATCH_TIMEOUT_MS"));
}

#[tokio::test]
async fn direct_write_dispatch_times_out_with_504() {
    ensure();
    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) =
        dispatch_into_async(encode("GET", "/slow", b""), &mut out).await
    else {
        panic!("504 wire must fit");
    };
    assert_eq!(split(&out[..n]).0["status"], 504);
}

#[tokio::test]
async fn streaming_deadline_before_commit_is_a_clean_504_header() {
    ensure();
    let mut header = Vec::new();
    let mut chunks = 0;
    let outcome = dispatch_streaming_with_header_async(
        encode("GET", "/slow", b""),
        |h: &[u8]| header.extend_from_slice(h),
        |_c: &[u8]| {
            chunks += 1;
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(split(&header).0["status"], 504);
    assert_eq!(chunks, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_deadline_does_not_cut_a_body_after_the_header_commit() {
    ensure();
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_streaming_with_header_async(
        encode("GET", "/slow_body", b""),
        |h: &[u8]| header.extend_from_slice(h),
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(split(&header).0["status"], 200);
    assert_eq!(body, b"headtail", "the deadline only bounds time-to-header");

    let mut body = Vec::new();
    let (header, outcome) =
        dispatch_streaming_with_outcome_async(encode("GET", "/slow_body", b""), |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        })
        .await;
    assert_eq!(outcome, StreamOutcome::Complete, "header-last path agrees");
    assert_eq!(split(&header).0["status"], 200);
    assert_eq!(body, b"headtail");

    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_bidirectional_streaming_with_header(
        encode("GET", "/slow_body", b""),
        || RequestChunk::End,
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete, "bidirectional agrees");
    assert_eq!(split(&header).0["status"], 200);
    assert_eq!(body, b"headtail");
}

#[tokio::test]
async fn buffered_dispatch_deadline_still_covers_the_whole_body() {
    ensure();
    let wire = dispatch_from_bytes_async(encode("GET", "/slow_body", b"")).await;
    assert_eq!(split(&wire).0["status"], 504);
    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) =
        dispatch_into_async(encode("GET", "/slow_body", b""), &mut out).await
    else {
        panic!("504 wire must fit");
    };
    assert_eq!(split(&out[..n]).0["status"], 504);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_deadline_before_commit_is_a_504_header() {
    ensure();
    let mut header = Vec::new();
    let outcome = dispatch_bidirectional_streaming_with_header(
        encode("GET", "/slow", b""),
        || RequestChunk::End,
        |_c: &[u8]| ControlFlow::Continue(()),
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(split(&header).0["status"], 504);
}

#[rstest]
#[case::exact_size_hint("/big_full")]
#[case::unknown_size_multi_frame("/big_stream")]
#[tokio::test]
async fn buffered_response_over_cap_becomes_500(#[case] path: &str) {
    ensure();
    let wire = dispatch_from_bytes_async(encode("GET", path, b"")).await;
    let (json, body) = split(&wire);
    assert_eq!(json["status"], 500);
    let msg = String::from_utf8_lossy(body);
    assert!(msg.contains("VESPERA_MAX_BUFFERED_RESPONSE_BYTES"), "{msg}");
    assert!(msg.contains(&MAX_RESPONSE.to_string()), "{msg}");

    let envelope = dispatch_owned(
        vespera_inprocess::Router::new().route("/big", get(big_stream)),
        RequestEnvelope {
            method: "GET".into(),
            path: "/big".into(),
            query: String::new(),
            headers: std::collections::HashMap::new(),
            body: String::new(),
        },
    )
    .await;
    assert_eq!(envelope.status, 500, "text-envelope path is capped too");
}

#[tokio::test]
async fn buffered_response_under_cap_is_untouched() {
    ensure();
    let wire = dispatch_from_bytes_async(encode("GET", "/small", b"")).await;
    let (json, body) = split(&wire);
    assert_eq!(json["status"], 200);
    assert_eq!(body, b"ok");
}

fn chunked_producer(total: usize) -> impl FnMut() -> RequestChunk + Send + 'static {
    let mut left = total;
    move || {
        if left == 0 {
            return RequestChunk::End;
        }
        let n = left.min(256);
        left -= n;
        RequestChunk::Data(vec![b'u'; n])
    }
}

#[rstest]
#[case::under_cap(MAX_STREAM_REQUEST, 200)]
#[case::over_cap(MAX_STREAM_REQUEST * 5, 413)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_request_body_cap(#[case] upload_len: usize, #[case] status: u64) {
    ensure();
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = dispatch_bidirectional_streaming_with_header(
        encode("POST", "/upload", b""),
        chunked_producer(upload_len),
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(split(&header).0["status"], status);
    if status == 200 {
        assert_eq!(body, upload_len.to_string().as_bytes());
    }
}

#[rstest]
#[case::over_cap("/big_422", 500)]
#[case::under_cap("/small_422", 422)]
#[tokio::test]
async fn buffered_and_direct_422_agree_on_the_response_cap(
    #[case] path: &str,
    #[case] status: u64,
) {
    ensure();
    let buffered = dispatch_from_bytes_async(encode("GET", path, b"")).await;
    let mut out = vec![0u8; 64 * 1024];
    let DirectWriteResult::Complete(n) =
        dispatch_into_async(encode("GET", path, b""), &mut out).await
    else {
        panic!("wire must fit");
    };
    assert_eq!(split(&buffered).0["status"], status);
    assert_eq!(
        buffered,
        &out[..n],
        "buffered and direct must produce byte-identical wire for {path}"
    );
}
