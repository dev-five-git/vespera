//! Client-disconnect cancellation on the streaming paths: once the chunk sink
//! reports a dead transport (`ControlFlow::Break` — the JNI push closure
//! returns it when the servlet `OutputStream` throws), the dispatch stops
//! polling the response body and DROPS it before returning, so the handler's
//! remaining work (the body stream and everything it owns) is released at
//! once instead of running to completion.
//!
//! Also covers a body that is WAITING (no next chunk yet): the header-first
//! paths send an empty-chunk flush hint before the body waits, and a dead
//! sink fails that flush — the waiting body is dropped without producing
//! another chunk.

use std::ops::ControlFlow;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::routing::get;
use bytes::Bytes;
use rstest::rstest;
use serde_json::json;
use vespera_inprocess::{
    RequestChunk, StreamOutcome, cancel, cancellable, dispatch_bidirectional_streaming_with_header,
    dispatch_streaming_with_header_async, dispatch_streaming_with_outcome_async,
    register_app_named,
};

const APP: &str = "cancel_on_disconnect";

/// One "dropped" flag per test key, flipped by the body stream's drop guard.
static DROPPED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
> = std::sync::LazyLock::new(Default::default);

fn flag(key: &str) -> Arc<AtomicBool> {
    Arc::clone(
        DROPPED
            .lock()
            .unwrap()
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(false))),
    )
}

struct DropGuard(Arc<AtomicBool>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// An endless body: one 1 KiB chunk per poll, never ends on its own.
async fn endless(Path(key): Path<String>) -> Body {
    let guard = DropGuard(flag(&key));
    let stream = futures_util::stream::unfold(guard, |guard| async move {
        tokio::task::yield_now().await;
        Some((
            Ok::<_, std::convert::Infallible>(Bytes::from(vec![b'x'; 1024])),
            guard,
        ))
    });
    Body::from_stream(stream)
}

/// One chunk, then the body waits forever (a slow producer).
async fn stalls(Path(key): Path<String>) -> Body {
    let guard = DropGuard(flag(&key));
    let stream = futures_util::stream::unfold((guard, false), |(guard, sent)| async move {
        if sent {
            std::future::pending::<()>().await;
        }
        Some((
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"first")),
            (guard, true),
        ))
    });
    Body::from_stream(stream)
}

fn install() {
    register_app_named(APP, || {
        Router::new()
            .route("/endless/{key}", get(endless))
            .route("/stalls/{key}", get(stalls))
    });
}

fn encode(path: &str) -> Vec<u8> {
    let header =
        serde_json::to_vec(&json!({ "v": 1, "method": "GET", "path": path, "app": APP })).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
    wire
}

/// A sink that accepts `ok_calls` non-empty chunks, then reports a dead
/// transport (every call afterwards, flush hints included, breaks).
fn dying_sink(ok_calls: usize, writes: Arc<AtomicUsize>) -> impl FnMut(&[u8]) -> ControlFlow<()> {
    move |chunk: &[u8]| {
        if chunk.is_empty() {
            return if writes.load(Ordering::SeqCst) >= ok_calls {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            };
        }
        if writes.fetch_add(1, Ordering::SeqCst) >= ok_calls {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

#[rstest]
#[case::endless_body("endless")]
#[case::waiting_body("stalls")]
#[tokio::test]
async fn header_first_streaming_drops_the_body_on_sink_failure(#[case] route: &str) {
    install();
    let key = format!("header-first-{route}");
    let dropped = flag(&key);
    let writes = Arc::new(AtomicUsize::new(0));
    let mut header_seen = false;
    let outcome = dispatch_streaming_with_header_async(
        encode(&format!("/{route}/{key}")),
        |_h: &[u8]| header_seen = true,
        dying_sink(1, Arc::clone(&writes)),
    )
    .await;
    assert!(header_seen);
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(dropped.load(Ordering::SeqCst), "body dropped before return");
}

#[tokio::test]
async fn header_last_streaming_drops_the_body_on_sink_failure() {
    install();
    let key = "header-last-endless";
    let dropped = flag(key);
    let writes = Arc::new(AtomicUsize::new(0));
    let (_, outcome) = dispatch_streaming_with_outcome_async(
        encode(&format!("/endless/{key}")),
        dying_sink(2, Arc::clone(&writes)),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(dropped.load(Ordering::SeqCst), "body dropped before return");
    assert_eq!(
        writes.load(Ordering::SeqCst),
        3,
        "no write after the failed one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_streaming_drops_the_body_on_sink_failure() {
    install();
    let key = "bidi-endless";
    let dropped = flag(key);
    let writes = Arc::new(AtomicUsize::new(0));
    let outcome = dispatch_bidirectional_streaming_with_header(
        encode(&format!("/endless/{key}")),
        || RequestChunk::End,
        dying_sink(1, Arc::clone(&writes)),
        |_h: &[u8]| {},
    )
    .await;
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(dropped.load(Ordering::SeqCst), "body dropped before return");
}

/// The async-dispatch cancellation primitive wrapped around a real dispatch:
/// cancelling the token drops the handler mid-flight.
#[tokio::test]
async fn cancelled_dispatch_drops_the_handler() {
    install();
    let key = "cancellable-stalls";
    let dropped = flag(key);
    // A sink that never fails: only the cancellation can stop this.
    let (token, fut) = cancellable(dispatch_streaming_with_header_async(
        encode(&format!("/stalls/{key}")),
        |_h: &[u8]| {},
        |_c: &[u8]| ControlFlow::Continue(()),
    ));
    let task = tokio::spawn(fut);
    // Let the handler reach its stall.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!task.is_finished());
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(cancel(token));
    assert_eq!(task.await.unwrap(), None);
    assert!(dropped.load(Ordering::SeqCst), "cancel dropped the body");
}
