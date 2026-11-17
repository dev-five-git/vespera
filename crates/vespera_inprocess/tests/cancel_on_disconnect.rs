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

/// Bound an await so a regression that stops the dispatch from finishing
/// fails the test instead of hanging CI.
async fn bounded<F: std::future::Future>(what: &str, fut: F) -> F::Output {
    tokio::time::timeout(std::time::Duration::from_secs(10), fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within 10 s"))
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

/// A sink that accepts every flush hint and `ok_calls` non-empty chunks,
/// then fails the next DATA write: the stop comes from `FrameBatch`
/// delivering a non-empty frame, never from a flush hint.
fn data_write_dying_sink(
    ok_calls: usize,
    writes: Arc<AtomicUsize>,
) -> impl FnMut(&[u8]) -> ControlFlow<()> {
    move |chunk: &[u8]| {
        if chunk.is_empty() || writes.fetch_add(1, Ordering::SeqCst) < ok_calls {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }
}

#[rstest]
// The endless body stops on a failed data write (flush hints accepted) ...
#[case::endless_body("endless", true)]
// ... the waiting one on the failed flush hint before it waits.
#[case::waiting_body("stalls", false)]
#[tokio::test]
async fn header_first_streaming_drops_the_body_on_sink_failure(
    #[case] route: &str,
    #[case] fail_on_data: bool,
) {
    install();
    let key = format!("header-first-{route}");
    let dropped = flag(&key);
    let writes = Arc::new(AtomicUsize::new(0));
    let mut header_seen = false;
    let wire = encode(&format!("/{route}/{key}"));
    let on_header = |_h: &[u8]| header_seen = true;
    let outcome = if fail_on_data {
        let sink = data_write_dying_sink(1, Arc::clone(&writes));
        bounded(
            route,
            dispatch_streaming_with_header_async(wire, on_header, sink),
        )
        .await
    } else {
        let sink = dying_sink(1, Arc::clone(&writes));
        bounded(
            route,
            dispatch_streaming_with_header_async(wire, on_header, sink),
        )
        .await
    };
    assert!(header_seen);
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(dropped.load(Ordering::SeqCst), "body dropped before return");
    if fail_on_data {
        assert_eq!(
            writes.load(Ordering::SeqCst),
            2,
            "no write after the failed one"
        );
    }
}

#[tokio::test]
async fn header_last_streaming_drops_the_body_on_sink_failure() {
    install();
    let key = "header-last-endless";
    let dropped = flag(key);
    let writes = Arc::new(AtomicUsize::new(0));
    let (_, outcome) = bounded(
        key,
        dispatch_streaming_with_outcome_async(
            encode(&format!("/endless/{key}")),
            dying_sink(2, Arc::clone(&writes)),
        ),
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
    let outcome = bounded(
        key,
        dispatch_bidirectional_streaming_with_header(
            encode(&format!("/endless/{key}")),
            || RequestChunk::End,
            data_write_dying_sink(1, Arc::clone(&writes)),
            |_h: &[u8]| {},
        ),
    )
    .await;
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert!(dropped.load(Ordering::SeqCst), "body dropped before return");
    assert_eq!(
        writes.load(Ordering::SeqCst),
        2,
        "stopped on the failed data write, nothing after it"
    );
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
    // Cancelling must finish the task (it resolves to None), not leave it
    // stalled; the bounded await turns a hang into a failure.
    assert_eq!(bounded(key, task).await.unwrap(), None);
    assert!(dropped.load(Ordering::SeqCst), "cancel dropped the body");
}
