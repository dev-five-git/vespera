//! Small-body fast path of bidirectional streaming (`streaming/inline_pull.rs`):
//! while the handler has not returned and no dispatch timeout is armed, a
//! dispatch driven directly by `block_on` pulls the first request chunks on
//! the calling thread instead of starting the `spawn_blocking` producer.
//!
//! Every case records which thread each pull ran on, so the tests pin both
//! the observable response AND whether the fast path (or the producer) was
//! taken.  This binary configures no dispatch timeout (the fast path is off
//! under a deadline — covered by `bidirectional_inline_deadline.rs`, its own
//! binary because the timeout is process-global).

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

thread_local! {
    /// Set while the spawned task below is being polled on this thread.
    static IN_SPAWNED_POLL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks every poll of the wrapped future in [`IN_SPAWNED_POLL`], so a pull
/// run synchronously inside that poll (inline) is told apart from one on the
/// producer's blocking-pool thread.
struct MarkPolls<F>(std::pin::Pin<Box<F>>);

impl<F: std::future::Future> std::future::Future for MarkPolls<F> {
    type Output = F::Output;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<F::Output> {
        IN_SPAWNED_POLL.with(|flag| flag.set(true));
        let poll = self.0.as_mut().poll(cx);
        IN_SPAWNED_POLL.with(|flag| flag.set(false));
        poll
    }
}

/// A dispatch running inside a spawned task must never pull inline (that
/// would block a runtime worker): the producer takes every pull.  Runs on a
/// MULTI-thread runtime, where only the `try_id` rule (not the
/// current-thread guard) keeps the pull off the worker (round 14).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_dispatch_never_pulls_inline() {
    ensure();
    let in_poll = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&in_poll);
    let mut script = vec![Pull::Data(64), Pull::End].into_iter();
    let pull = move || {
        seen.lock()
            .unwrap()
            .push(IN_SPAWNED_POLL.with(std::cell::Cell::get));
        match script.next() {
            Some(Pull::Data(n)) => RequestChunk::Data(vec![b'x'; n]),
            _ => RequestChunk::End,
        }
    };
    let (got_status, body) = tokio::spawn(MarkPolls(Box::pin(async move {
        let mut body = Vec::new();
        let header = dispatch_bidirectional_streaming_closing(
            encode("/echo"),
            pull,
            |chunk: &[u8]| {
                body.extend_from_slice(chunk);
                ControlFlow::Continue(())
            },
            || {},
        )
        .await;
        (status(&header), body)
    })))
    .await
    .unwrap();
    assert_eq!(got_status, 200);
    assert_eq!(body.len(), 64);
    assert_eq!(
        *in_poll.lock().unwrap(),
        [false, false],
        "every pull runs on the producer, none inside the spawned task's poll"
    );
}

/// Round 12: a current-thread runtime's `block_on` thread is its only
/// worker, so the fast path is off there.  The pull below waits on a
/// `std::sync::mpsc` fed by a task of that same runtime: inline it would
/// block the only thread that can run the sender — a permanent hang (the
/// watchdog turns it into a failure).
#[test]
fn current_thread_block_on_never_pulls_inline() {
    ensure();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let caller = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let pulled_on = Arc::new(Mutex::new(Vec::new()));
        let threads = Arc::clone(&pulled_on);
        let rx = Mutex::new(rx);
        let pull = move || {
            threads.lock().unwrap().push(std::thread::current().id());
            rx.lock()
                .unwrap()
                .recv()
                .map_or(RequestChunk::End, RequestChunk::Data)
        };
        let (got_status, body) = runtime.block_on(async move {
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                tx.send(vec![b'q'; 7]).unwrap();
            });
            let mut body = Vec::new();
            let header = dispatch_bidirectional_streaming_closing(
                encode("/echo"),
                pull,
                |chunk: &[u8]| {
                    body.extend_from_slice(chunk);
                    ControlFlow::Continue(())
                },
                || {},
            )
            .await;
            (status(&header), body)
        });
        let inline = pulled_on
            .lock()
            .unwrap()
            .iter()
            .filter(|t| **t == caller)
            .count();
        done_tx.send((got_status, body, inline)).unwrap();
    });
    let (got_status, body, inline) = done_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("an inline pull hung the current-thread runtime");
    assert_eq!(got_status, 200);
    assert_eq!(body, b"qqqqqqq");
    assert_eq!(
        inline, 0,
        "no pull may run on the current-thread block_on thread"
    );
}

/// Round 12: an inline pull runs under `block_in_place` (still inside the
/// runtime context: `Handle::current()` works), so Tokio's blocking APIs work
/// in it exactly as on the `spawn_blocking` producer
/// (before, `blocking_recv` panicked "Cannot block the current thread from
/// within a runtime" and every upload failed with a body error `400`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_pull_may_use_tokio_blocking_apis() {
    ensure();
    let caller = std::thread::current().id();
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    tx.send(vec![b'z'; 5]).await.unwrap();
    drop(tx);
    let pulled_on = Arc::new(Mutex::new(Vec::new()));
    let threads = Arc::clone(&pulled_on);
    let rx = Mutex::new(rx);
    let pull = move || {
        threads.lock().unwrap().push(std::thread::current().id());
        let handle = tokio::runtime::Handle::current();
        let chunk = rx.lock().unwrap().blocking_recv();
        // `Handle::block_on` works too (as on a blocking-pool thread).
        handle.block_on(tokio::task::yield_now());
        chunk.map_or(RequestChunk::End, RequestChunk::Data)
    };
    let mut body = Vec::new();
    let header = dispatch_bidirectional_streaming_closing(
        encode("/echo"),
        pull,
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        || {},
    )
    .await;
    assert_eq!(status(&header), 200);
    assert_eq!(body, b"zzzzz");
    let inline = pulled_on
        .lock()
        .unwrap()
        .iter()
        .filter(|t| **t == caller)
        .count();
    assert_eq!(inline, 2, "both pulls ran inline on the block_on thread");
}
