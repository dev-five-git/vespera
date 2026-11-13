//! Diagnostic routes for the JNI E2E suite: a response of a chosen size (the
//! buffered-response cap), a deliberately slow handler (the Spring async
//! timeout / async cancellation) and an endless trickling stream (streaming
//! client disconnect). Each records under a caller-chosen key how often it
//! finished — and, when dropped before finishing, `"<key>:dropped"` — so a
//! test can prove the handler ran exactly once, or was cancelled. Sizes and
//! delays are clamped so the routes stay harmless.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError},
    time::Duration,
};

use vespera::axum::{
    body::{Body, Bytes},
    extract::Path,
    http::header,
    response::{IntoResponse, Response},
};

/// Largest body `/diag/blob` produces (16 MiB).
pub const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;
/// Longest delay `/diag/sleep` waits (60 s).
pub const MAX_SLEEP_MILLIS: u64 = 60_000;
/// Most chunks `/diag/drip` emits before ending on its own.
pub const MAX_DRIP_CHUNKS: u64 = 10_000;

static RUNS: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Mutex::default);

fn record(key: String) {
    *RUNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_default() += 1;
}

fn count(key: &str) -> u64 {
    RUNS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .copied()
        .unwrap_or_default()
}

/// Records `"<key>:dropped"` when dropped while still armed — i.e. the
/// handler (or its body stream) was cancelled before it finished.
struct DropProbe(Option<String>);

impl DropProbe {
    const fn new(key: String) -> Self {
        Self(Some(key))
    }

    /// The work finished: dropping no longer counts as a cancellation.
    fn disarm(&mut self) -> Option<String> {
        self.0.take()
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        if let Some(key) = self.0.take() {
            record(format!("{key}:dropped"));
        }
    }
}

/// `bytes` bytes of `x` (at most [`MAX_BLOB_BYTES`]); counts a run of `key`.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/blob/{key}/{bytes}", tags = ["diag"])]
pub async fn blob(Path((key, bytes)): Path<(String, usize)>) -> Vec<u8> {
    record(key);
    vec![b'x'; bytes.min(MAX_BLOB_BYTES)]
}

/// Answers `slept` after `ms` milliseconds (at most [`MAX_SLEEP_MILLIS`]);
/// records `"<key>:started"` as soon as the handler body runs, then counts a
/// run of `key` once the delay is over, or `"<key>:dropped"` when the handler
/// is cancelled while sleeping.
///
/// The `:started` marker lets a test cancel only once the handler really
/// runs: a `dispatchAsync` holds its in-flight slot from admission, before a
/// Tokio worker first polls the task, and a cancel landing in that window
/// drops the request before the handler (and its drop probe) ever existed.
#[vespera::route(get, path = "/sleep/{key}/{ms}", tags = ["diag"])]
pub async fn sleep(Path((key, ms)): Path<(String, u64)>) -> &'static str {
    record(format!("{key}:started"));
    let mut probe = DropProbe::new(key);
    tokio::time::sleep(Duration::from_millis(ms.min(MAX_SLEEP_MILLIS))).await;
    if let Some(key) = probe.disarm() {
        record(key);
    }
    "slept"
}

/// A `text/plain` stream of `chunks` 1 KiB chunks (at most
/// [`MAX_DRIP_CHUNKS`]), one every `ms` milliseconds (at most 1 s).  Counts a
/// run of `key` when the last chunk was produced, or `"<key>:dropped"` when
/// the body is dropped before — the client went away and the proxy's write
/// failed.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/drip/{key}/{chunks}/{ms}", tags = ["diag"])]
pub async fn drip(Path((key, chunks, ms)): Path<(String, u64, u64)>) -> Body {
    let chunks = chunks.min(MAX_DRIP_CHUNKS);
    let every = Duration::from_millis(ms.min(1_000));
    let stream = futures_util::stream::unfold(
        (DropProbe::new(key), 0u64),
        move |(mut probe, n)| async move {
            if n >= chunks {
                if let Some(key) = probe.disarm() {
                    record(key);
                }
                return None;
            }
            if n > 0 {
                tokio::time::sleep(every).await;
            }
            let chunk = Bytes::from(vec![b'd'; 1024]);
            Some((Ok::<_, std::convert::Infallible>(chunk), (probe, n + 1)))
        },
    );
    Body::from_stream(stream)
}

/// How many runs of `key` finished (`"<key>:dropped"` counts cancellations).
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/runs/{key}", tags = ["diag"])]
pub async fn runs(Path(key): Path<String>) -> String {
    count(&key).to_string()
}

/// `dispatchAsync` calls currently holding an in-flight slot (JNI builds
/// only; `0` in the standalone server).
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/async-in-flight", tags = ["diag"])]
pub async fn async_in_flight() -> String {
    vespera::jni::async_in_flight().to_string()
}

/// Reads the whole request body, then panics — BEFORE any response header
/// exists.  Paired with a large `POST` the Spring proxy's smart resolver
/// routes it through bidirectional streaming (`dispatchFullStreamingWithHeader`),
/// where the request producer has already been started: the proxy must
/// answer `500` (panicked), never a `200`.
#[allow(clippy::panic)]
#[vespera::route(post, path = "/panic-after-read", tags = ["diag"])]
pub async fn panic_after_read(body: Body) -> String {
    let read = vespera::axum::body::to_bytes(body, usize::MAX)
        .await
        .map_or(0, |bytes| bytes.len());
    panic!("intentional panic after reading {read} request bytes");
}

/// A `200` `application/octet-stream` body of `chunks` 1 KiB chunks (at most
/// [`MAX_DRIP_CHUNKS`]) whose stream then panics — AFTER the response header
/// was produced.  The request body (if any) is drained first, so a large
/// `POST` exercises bidirectional streaming.  A client must never see this
/// as a complete `200`: the proxy answers `500` while the response is still
/// uncommitted, and aborts the connection once bytes reached the client.
#[allow(clippy::panic)]
#[vespera::route(post, path = "/panic-mid-body/{chunks}", tags = ["diag"])]
pub async fn panic_mid_body(Path(chunks): Path<u64>, body: Body) -> Response {
    let _ = vespera::axum::body::to_bytes(body, usize::MAX).await;
    let chunks = chunks.min(MAX_DRIP_CHUNKS);
    let stream = futures_util::stream::unfold(0u64, move |n| async move {
        assert!(
            n < chunks,
            "intentional panic after {chunks} response chunks"
        );
        let chunk = Bytes::from(vec![b'p'; 1024]);
        Some((Ok::<_, std::convert::Infallible>(chunk), n + 1))
    });
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}
