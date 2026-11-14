//! A bidirectional request body that OUTLIVES its dispatch — the handler
//! moved it into a detached `tokio::spawn` task and responded at once — must
//! never read the host request source after the dispatch returned.  On the
//! JNI path that source is a Java `InputStream` of a native call that has
//! already returned; reading it would race the next request on the same
//! (pooled) stream.  The late read fails the body with an error instead.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming_closing, register_app_named,
};

const APP: &str = "bidirectional_detached_body";

/// Released by the test once the dispatch returned.
fn dispatch_returned() -> &'static Notify {
    static NOTIFY: OnceLock<Notify> = OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

/// What the detached task saw when it finally read the body.
type Outcome = oneshot::Sender<Result<usize, String>>;

fn outcome_slot() -> &'static Mutex<Option<Outcome>> {
    static SLOT: OnceLock<Mutex<Option<Outcome>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Responds immediately; a detached task reads the body only after the
/// dispatch returned.
async fn detach(body: Body) -> &'static str {
    let notified = dispatch_returned().notified();
    tokio::spawn(async move {
        notified.await;
        let read = body
            .collect()
            .await
            .map(|collected| collected.to_bytes().len())
            .map_err(|err| err.to_string());
        if let Some(tx) = outcome_slot().lock().unwrap().take() {
            let _ = tx.send(read);
        }
    });
    "detached"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_read_after_the_dispatch_returned_never_touches_the_source() {
    register_app_named(APP, || Router::new().route("/detach", post(detach)));
    let (tx, rx) = oneshot::channel();
    *outcome_slot().lock().unwrap() = Some(tx);

    let pulls = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let pulls_in_source = Arc::clone(&pulls);
    let closes_in_hook = Arc::clone(&closes);
    let mut response = Vec::new();
    let header = dispatch_bidirectional_streaming_closing(
        encode("/detach"),
        move || {
            pulls_in_source.fetch_add(1, Ordering::SeqCst);
            RequestChunk::Data(b"late".to_vec())
        },
        |chunk: &[u8]| {
            response.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move || {
            closes_in_hook.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;
    assert_eq!(status(&header), 200);
    assert_eq!(response, b"detached");

    dispatch_returned().notify_one();
    let read = rx.await.expect("the detached task reports its read");
    assert!(read.is_err(), "a late read fails the body, got {read:?}");
    assert_eq!(
        pulls.load(Ordering::SeqCst),
        0,
        "the source is never called"
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        0,
        "nothing was read, nothing to close"
    );
}
