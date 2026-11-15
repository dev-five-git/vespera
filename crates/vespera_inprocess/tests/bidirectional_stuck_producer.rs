//! A started request producer whose body OUTLIVES the dispatch without
//! being polled again — the handler moved it into a detached task that
//! read one frame and then parked — must not stay blocked on the full
//! channel forever: once the dispatch finished its send gives up, the
//! sender drops and the producer exits.  Before, the dispatch waited out
//! the whole `REQUEST_PRODUCER_JOIN_GRACE` and the detached producer kept a
//! blocking thread, a producer slot and the host pull closure forever.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming_closing, register_app_named,
};

const APP: &str = "bidirectional_stuck_producer";

/// Released by test `slot` at its end so its parked task exits.  One per
/// test: the tests run in parallel, and a shared one would let the first
/// test to finish release (drop) the OTHER test's body early.
fn release(slot: usize) -> &'static Notify {
    static NOTIFY: [OnceLock<Notify>; 2] = [OnceLock::new(), OnceLock::new()];
    NOTIFY[slot].get_or_init(Notify::new)
}

/// Reads one frame in a detached task (starting the producer), then keeps
/// the body unpolled until `release(slot)`; responds once that first frame
/// arrived.
async fn park_until(body: Body, slot: usize) -> &'static str {
    let (first_tx, first_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut body = body;
        let _ = first_tx.send(body.frame().await.is_some());
        release(slot).notified().await;
        drop(body);
    });
    assert!(first_rx.await.unwrap(), "the first frame arrives");
    "parked"
}

fn app() -> Router {
    Router::new()
        .route("/park0", post(|body: Body| park_until(body, 0)))
        .route("/park1", post(|body: Body| park_until(body, 1)))
}

fn encode(path: &str) -> Vec<u8> {
    let header = json!({ "v": 1, "method": "POST", "path": path, "app": APP });
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

/// Flags its own drop: the producer releases the pull closure on exit.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_producer_blocked_on_an_unpolled_body_exits_once_the_dispatch_finished() {
    register_app_named(APP, app);
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&dropped));
    let mut response = Vec::new();
    let started = Instant::now();
    let header = dispatch_bidirectional_streaming_closing(
        encode("/park0"),
        move || {
            let _keep = &flag;
            RequestChunk::Data(vec![b'x'; 1024])
        },
        |chunk: &[u8]| {
            response.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        || {},
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status(&header), 200);
    assert_eq!(response, b"parked");
    assert!(
        elapsed < Duration::from_millis(1500),
        "the dispatch did not wait out the join grace: {elapsed:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dropped.load(Ordering::SeqCst) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "the producer exited and released the pull closure"
    );
    release(0).notify_one();
}

/// A producer parked INSIDE a source read when the dispatch finishes (the
/// close hook unblocks it with one more chunk, which still fits the
/// channel) never calls the source again: the loop stops before the next
/// pull.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_producer_unblocked_after_the_dispatch_finished_never_pulls_again() {
    register_app_named(APP, app);
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&dropped));
    let pulls = Arc::new(AtomicUsize::new(0));
    let pulls_in_source = Arc::clone(&pulls);
    let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
    let mut response = Vec::new();
    let header = dispatch_bidirectional_streaming_closing(
        encode("/park1"),
        move || {
            let _keep = &flag;
            if pulls_in_source.fetch_add(1, Ordering::SeqCst) == 1 {
                // Parked until the request source is closed.
                let _ = closed_rx.recv_timeout(Duration::from_secs(5));
            }
            RequestChunk::Data(b"x".to_vec())
        },
        |chunk: &[u8]| {
            response.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move || {
            let _ = closed_tx.send(());
        },
    )
    .await;
    assert_eq!(status(&header), 200);
    assert_eq!(response, b"parked");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dropped.load(Ordering::SeqCst) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(dropped.load(Ordering::SeqCst), "the producer exited");
    assert_eq!(
        pulls.load(Ordering::SeqCst),
        2,
        "no pull after the dispatch finished"
    );
    release(1).notify_one();
}
