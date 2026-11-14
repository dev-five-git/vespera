//! The small-body fast path of bidirectional streaming
//! (`streaming/inline_pull.rs`) is OFF while a dispatch timeout is armed: an
//! inline read blocks inside `poll_frame`, where the deadline could not fire.
//!
//! Its own binary because the dispatch timeout is process-global and
//! first-wins (`bidirectional_inline.rs` needs it unset).  The dispatch is
//! driven by a plain `Runtime::block_on` — no task id, every other fast-path
//! condition met — so the deadline is the only thing keeping the pulls off
//! the calling thread.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use bytes::Bytes;
use serde_json::{Value, json};
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming, register_app_named, set_dispatch_timeout,
};

const APP: &str = "bidirectional_inline_deadline";

fn encode() -> Vec<u8> {
    let header = json!({
        "v": 1, "method": "POST", "path": "/echo", "app": APP,
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

#[test]
fn no_pull_runs_on_the_block_on_thread_under_a_deadline() {
    assert!(set_dispatch_timeout(Duration::from_secs(30)));
    register_app_named(APP, || {
        Router::new().route("/echo", post(|body: Bytes| async move { body }))
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let recorder = Arc::clone(&threads);
    let mut chunks = vec![RequestChunk::End, RequestChunk::Data(vec![b'x'; 64])];
    let pull = move || {
        recorder.lock().unwrap().push(std::thread::current().id());
        chunks.pop().unwrap_or(RequestChunk::End)
    };
    let mut body = Vec::new();
    let header = runtime.block_on(dispatch_bidirectional_streaming(
        encode(),
        pull,
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    ));

    assert_eq!(status(&header), 200);
    assert_eq!(body, vec![b'x'; 64]);
    let threads = threads.lock().unwrap();
    assert_eq!(threads.len(), 2, "pull count");
    let caller = std::thread::current().id();
    assert!(
        threads.iter().all(|t| *t != caller),
        "a pull ran inline on the block_on thread despite the armed deadline"
    );
}
