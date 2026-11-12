//! A bidirectional dispatch must not hang after its response completed just
//! because the request producer is parked in a blocking source read that the
//! close hook cannot interrupt (e.g. a Java `InputStream.close()` issued from
//! another thread that does not unblock a pending `read`).  The producer join
//! is bounded by `REQUEST_PRODUCER_JOIN_GRACE`, after which the producer is
//! detached and the completed response is returned.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::json;
use vespera_inprocess::{
    REQUEST_PRODUCER_JOIN_GRACE, RequestChunk, StreamOutcome,
    dispatch_bidirectional_streaming_with_header_closing, register_app_named,
};

const APP: &str = "producer_join_bounded";

/// Reads exactly one request chunk, then answers without draining the rest.
async fn first_chunk_only(body: Body) -> String {
    let mut body = body;
    let first = body.frame().await;
    format!("got-first={}", first.is_some())
}

fn encode() -> Vec<u8> {
    let header = json!({ "v": 1, "method": "POST", "path": "/first", "app": APP });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = u32::try_from(header_bytes.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(&header_bytes);
    wire
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_response_is_not_held_hostage_by_a_stuck_producer() {
    register_app_named(APP, || {
        Router::new().route("/first", post(first_chunk_only))
    });

    // The producer hands out one chunk, then parks in a "read" that the close
    // hook below cannot interrupt (it only unparks 2 s after the grace, so the
    // blocking thread is eventually released and the test leaves no thread
    // behind forever).
    let released = Arc::new(AtomicBool::new(false));
    let released_by_producer = Arc::clone(&released);
    let mut calls = 0u32;
    let pull = move || {
        calls += 1;
        if calls == 1 {
            return RequestChunk::Data(b"chunk".to_vec());
        }
        std::thread::sleep(REQUEST_PRODUCER_JOIN_GRACE + Duration::from_secs(2));
        released_by_producer.store(true, Ordering::Release);
        RequestChunk::End
    };

    let mut header = Vec::new();
    let mut body = Vec::new();
    let started = Instant::now();
    let outcome = dispatch_bidirectional_streaming_with_header_closing(
        encode(),
        pull,
        |c: &[u8]| {
            body.extend_from_slice(c);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
        // A close that does NOT unblock the parked read.
        || {},
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(outcome, StreamOutcome::Complete);
    assert!(
        String::from_utf8_lossy(&header).contains(r#""status":200"#),
        "header delivered"
    );
    assert_eq!(body, b"got-first=true");
    assert!(
        elapsed < REQUEST_PRODUCER_JOIN_GRACE + Duration::from_millis(1500),
        "dispatch must return after the bounded join grace, took {elapsed:?}"
    );
    assert!(
        !released.load(Ordering::Acquire),
        "the stuck producer was detached, not awaited"
    );
}
