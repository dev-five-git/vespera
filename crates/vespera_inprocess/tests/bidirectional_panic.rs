//! Handler panics on the bidirectional header-first path, driven exactly
//! like the JNI symbol drives it: `Runtime::block_on` on a host thread inside
//! `catch_unwind`.  The JNI landing pad turns the unwind into the response
//! status, and it relies on two preconditions pinned here for BOTH request
//! body paths — the inline small-body pull (`streaming/inline_pull.rs`) and
//! the spawned request producer:
//!
//! * a panic **before** the response header (in the handler, after it read
//!   the body) unwinds out of the dispatch with `on_header` never called, so
//!   the JNI fallback answers a `500` with `"panicked":"true"`;
//! * a panic **after** the header (in the response body stream) unwinds out
//!   with `on_header` called exactly once — never a returned
//!   [`StreamOutcome`] that would let the host finish a short `200` as if it
//!   were complete — so the JNI side throws the panicked abort exception.
//!
//! The request source is closed on every panic (no parked producer leaks).

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use axum::Router;
use axum::body::Body;
use axum::response::Response;
use axum::routing::post;
use bytes::Bytes;
use rstest::rstest;
use serde_json::{Value, json};
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming_with_header_closing, register_app_named,
    set_streaming_chunk_bytes,
};

const APP: &str = "bidirectional_panic";
const CHUNK: usize = 4096;

fn ensure() {
    let _ = set_streaming_chunk_bytes(CHUNK);
    register_app_named(APP, || {
        Router::new()
            .route("/read-then-panic", post(read_then_panic))
            .route("/body-panic", post(body_panic))
    });
}

#[expect(clippy::panic, reason = "the panic is the behaviour under test")]
async fn read_then_panic(body: Bytes) -> String {
    panic!("intentional panic after reading {} bytes", body.len());
}

/// Reads the body, then answers `200` with two ready frames followed by a
/// panicking poll.
async fn body_panic(body: Bytes) -> Response {
    let _ = body.len();
    let stream = futures_util::stream::unfold(0u8, |n| async move {
        assert!(n < 2, "intentional body-stream panic");
        Some((
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"part")),
            n + 1,
        ))
    });
    Response::new(Body::from_stream(stream))
}

fn encode(path: &str) -> Vec<u8> {
    let header = json!({
        "v": 1, "method": "POST", "path": path, "app": APP,
        "headers": { "content-type": "application/octet-stream" },
    });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = u32::try_from(header_bytes.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(&header_bytes);
    wire
}

fn status(wire: &[u8]) -> u64 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    header["status"].as_u64().unwrap()
}

/// What one panicking dispatch observed.
struct Observed {
    panicked: bool,
    headers: Vec<Vec<u8>>,
    body: Vec<u8>,
    pull_threads: Vec<ThreadId>,
    closes: usize,
}

fn dispatch(path: &str, chunks: usize) -> Observed {
    ensure();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let pull_threads = Arc::new(Mutex::new(Vec::new()));
    let close_count = Arc::new(AtomicUsize::new(0));
    let headers = Mutex::new(Vec::new());
    let body = Mutex::new(Vec::new());

    let pulled = Arc::clone(&pull_threads);
    let mut sent = 0;
    let pull = move || {
        pulled.lock().unwrap().push(std::thread::current().id());
        if sent < chunks {
            sent += 1;
            RequestChunk::Data(vec![b'x'; CHUNK])
        } else {
            RequestChunk::End
        }
    };
    let closed = Arc::clone(&close_count);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(dispatch_bidirectional_streaming_with_header_closing(
            encode(path),
            pull,
            |chunk: &[u8]| {
                body.lock().unwrap().extend_from_slice(chunk);
                ControlFlow::Continue(())
            },
            |header: &[u8]| headers.lock().unwrap().push(header.to_vec()),
            move || {
                closed.fetch_add(1, Ordering::SeqCst);
            },
        ))
    }));
    drop(runtime);
    let pull_threads = pull_threads.lock().unwrap().clone();
    Observed {
        panicked: outcome.is_err(),
        headers: headers.into_inner().unwrap(),
        body: body.into_inner().unwrap(),
        pull_threads,
        closes: close_count.load(Ordering::SeqCst),
    }
}

/// Request bodies: one chunk (pulled inline on the `block_on` thread) and
/// eight chunks (the inline budget is spent, the producer takes the rest).
#[rstest]
#[case::inline_pull(1, true)]
#[case::producer(8, false)]
fn panic_before_the_header_never_delivers_one(#[case] chunks: usize, #[case] all_inline: bool) {
    let seen = dispatch("/read-then-panic", chunks);
    assert!(seen.panicked, "the handler panic must unwind to the host");
    assert!(
        seen.headers.is_empty(),
        "no header before the panic: the host answers its own 500"
    );
    assert_eq!(seen.body, b"");
    assert_eq!(
        seen.pull_threads.len(),
        chunks + 1,
        "the whole body was read"
    );
    let here = std::thread::current().id();
    assert_eq!(
        seen.pull_threads.iter().all(|t| *t == here),
        all_inline,
        "inline pulls run on the block_on caller"
    );
    assert_eq!(seen.closes, 1, "the request source is closed on the panic");
}

#[rstest]
#[case::inline_pull(1)]
#[case::producer(8)]
fn panic_after_the_header_unwinds_instead_of_completing(#[case] chunks: usize) {
    let seen = dispatch("/body-panic", chunks);
    assert!(
        seen.panicked,
        "a body panic must unwind, never return a clean StreamOutcome"
    );
    assert_eq!(seen.headers.len(), 1, "the 200 header was delivered once");
    assert_eq!(status(&seen.headers[0]), 200);
    assert!(
        seen.body.len() < 3 * 4,
        "never more than the frames produced before the panic"
    );
    assert_eq!(seen.closes, 1, "the request source is closed on the panic");
}
