//! Small-body fast path of bidirectional streaming (`streaming/inline_pull.rs`):
//! while the handler has not returned and no dispatch timeout is armed, a
//! dispatch driven directly by `block_on` pulls the first request chunks on
//! the calling thread instead of starting the `spawn_blocking` producer.
//!
//! Every case records which thread each pull ran on, so the tests pin both
//! the observable response AND whether the fast path (or the producer) was
//! taken.  This binary configures no dispatch timeout (the fast path is off
//! under a deadline — covered by `dispatch_limits.rs`).

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use axum::Router;
use axum::body::Body;
use axum::routing::post;
use bytes::Bytes;
use http_body_util::BodyExt;
use rstest::rstest;
use serde_json::{Value, json};
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming_closing, register_app_named,
    set_max_streaming_request_bytes, set_streaming_chunk_bytes,
};

const APP: &str = "bidirectional_inline";
const CHUNK: usize = 4096;
const MAX_STREAM_REQUEST: usize = 10_000;

fn ensure() {
    let _ = set_streaming_chunk_bytes(CHUNK);
    let _ = set_max_streaming_request_bytes(MAX_STREAM_REQUEST);
    register_app_named(APP, || {
        Router::new()
            .route("/echo", post(|body: Bytes| async move { body }))
            .route("/ignore", post(|| async { "ignored" }))
            .route("/first_frame", post(first_frame))
            .route("/stream_echo", post(|body: Body| async move { body }))
    });
}

/// Reads ONE request frame, then responds — the rest of the upload is
/// never requested.
async fn first_frame(mut body: Body) -> String {
    let frame = body.frame().await.and_then(Result::ok);
    frame
        .and_then(|f| f.into_data().ok())
        .map_or(0, |d| d.len())
        .to_string()
}

/// One scripted pull.
#[derive(Clone, Copy, Debug)]
enum Pull {
    Data(usize),
    End,
    Error,
    Panic,
}

/// Records how many pulls ran and on which threads.
#[derive(Clone, Default)]
struct Probe {
    threads: Arc<Mutex<Vec<ThreadId>>>,
    closes: Arc<AtomicUsize>,
}

impl Probe {
    fn pull(&self, script: &[Pull]) -> impl FnMut() -> RequestChunk + Send + 'static {
        let script = script.to_vec();
        let mut next = 0;
        let threads = Arc::clone(&self.threads);
        move || {
            threads.lock().unwrap().push(std::thread::current().id());
            let step = script.get(next).copied().unwrap_or(Pull::End);
            next += 1;
            match step {
                Pull::Data(n) => RequestChunk::Data(vec![b'x'; n]),
                Pull::End => RequestChunk::End,
                Pull::Error => RequestChunk::Error,
                Pull::Panic => panic!("intentional pull panic"),
            }
        }
    }

    fn pulls(&self) -> usize {
        self.threads.lock().unwrap().len()
    }

    /// Pulls that ran on `thread` (i.e. inline on the `block_on` caller).
    fn pulls_on(&self, thread: ThreadId) -> usize {
        self.threads
            .lock()
            .unwrap()
            .iter()
            .filter(|t| **t == thread)
            .count()
    }
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

async fn run(path: &str, script: &[Pull], probe: &Probe) -> (u64, Vec<u8>) {
    let mut body = Vec::new();
    let closes = Arc::clone(&probe.closes);
    let header = dispatch_bidirectional_streaming_closing(
        encode(path),
        probe.pull(script),
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move || {
            closes.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;
    (status(&header), body)
}

#[rstest]
#[case::empty_body("/echo", &[Pull::End], 200, Some(0), 1, 1)]
#[case::under_chunk("/echo", &[Pull::Data(64), Pull::End], 200, Some(64), 2, 2)]
#[case::exactly_chunk("/echo", &[Pull::Data(CHUNK), Pull::End], 200, Some(CHUNK), 2, 2)]
#[case::over_chunk(
    "/echo",
    &[Pull::Data(CHUNK), Pull::Data(CHUNK), Pull::Data(10), Pull::End],
    200,
    Some(2 * CHUNK + 10),
    4,
    2
)]
#[case::at_limit("/echo", &[Pull::Data(MAX_STREAM_REQUEST), Pull::End], 200, Some(MAX_STREAM_REQUEST), 2, 2)]
#[case::over_limit_in_first_chunk("/echo", &[Pull::Data(MAX_STREAM_REQUEST + 1)], 413, None, 1, 1)]
#[case::error_on_first_pull("/echo", &[Pull::Error], 400, None, 1, 1)]
#[case::panic_on_first_pull("/echo", &[Pull::Panic], 400, None, 1, 1)]
// The budget is spent on two data chunks, so the spawned producer takes the
// failing third pull and aborts the body the same way.
#[case::error_after_the_inline_budget(
    "/echo",
    &[Pull::Data(64), Pull::Data(64), Pull::Error],
    400,
    None,
    3,
    2
)]
#[case::empty_read_then_end("/echo", &[Pull::Data(0), Pull::End], 200, Some(0), 2, 2)]
#[case::empty_reads_exhaust_the_budget(
    "/echo",
    &[Pull::Data(0), Pull::Data(0), Pull::Data(3), Pull::End],
    200,
    Some(3),
    4,
    2
)]
#[case::early_response_after_one_frame(
    "/first_frame",
    &[Pull::Data(64), Pull::Data(64), Pull::End],
    200,
    None,
    1,
    1
)]
#[case::body_ignored("/ignore", &[Pull::Data(64), Pull::End], 200, None, 0, 0)]
// Polled by the streaming RESPONSE (after the handler returned): never
// inline, so an interactive client is never blocked on.
#[case::polled_after_the_response("/stream_echo", &[Pull::Data(64), Pull::End], 200, Some(64), 2, 0)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_bodies_are_pulled_inline_on_the_block_on_thread(
    #[case] path: &str,
    #[case] script: &[Pull],
    #[case] expected_status: u64,
    #[case] echoed: Option<usize>,
    #[case] pulls: usize,
    #[case] inline_pulls: usize,
) {
    ensure();
    let probe = Probe::default();
    let (got_status, body) = run(path, script, &probe).await;
    assert_eq!(got_status, expected_status);
    if let Some(len) = echoed {
        assert_eq!(body.len(), len);
    }
    assert_eq!(probe.pulls(), pulls, "pull count");
    assert_eq!(
        probe.pulls_on(std::thread::current().id()),
        inline_pulls,
        "pulls on the block_on thread"
    );
    // The source is closed after the response iff it was read at all.
    assert_eq!(probe.closes.load(Ordering::SeqCst), usize::from(pulls > 0));
}

/// A dispatch running inside a spawned task must never pull inline (that
/// would block a runtime worker): the producer takes every pull.
#[tokio::test(flavor = "current_thread")]
async fn spawned_dispatch_never_pulls_inline() {
    ensure();
    let probe = Probe::default();
    let task_probe = probe.clone();
    let (got_status, body) =
        tokio::spawn(async move { run("/echo", &[Pull::Data(64), Pull::End], &task_probe).await })
            .await
            .unwrap();
    assert_eq!(got_status, 200);
    assert_eq!(body.len(), 64);
    assert_eq!(probe.pulls(), 2);
    assert_eq!(
        probe.pulls_on(std::thread::current().id()),
        0,
        "current-thread runtime: the spawned task runs here, pulls must not"
    );
}
