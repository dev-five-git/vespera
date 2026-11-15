//! A started request producer whose body OUTLIVES the dispatch without
//! being polled again — the handler moved it into a detached task that
//! read one frame and then parked — must not stay blocked on the full
//! channel forever: once the dispatch finished its send gives up, the
//! sender drops and the producer exits.  Before, the dispatch waited out
//! the whole `REQUEST_PRODUCER_JOIN_GRACE` and the detached producer kept a
//! blocking thread, a producer slot and the host pull closure forever.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Released by the test at the end so the parked task exits.
fn release() -> &'static Notify {
    static NOTIFY: OnceLock<Notify> = OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

/// Reads one frame in a detached task (starting the producer), then keeps
/// the body unpolled; responds once that first frame arrived.
async fn park(body: Body) -> &'static str {
    let (first_tx, first_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut body = body;
        let _ = first_tx.send(body.frame().await.is_some());
        release().notified().await;
        drop(body);
    });
    assert!(first_rx.await.unwrap(), "the first frame arrives");
    "parked"
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
    register_app_named(APP, || Router::new().route("/park", post(park)));
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&dropped));
    let mut response = Vec::new();
    let started = Instant::now();
    let header = dispatch_bidirectional_streaming_closing(
        encode("/park"),
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
    release().notify_one();
}
