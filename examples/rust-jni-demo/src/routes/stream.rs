//! Streaming / overflow probes for the JNI end-to-end tests.
//!
//! * `/stream/events` — a `text/event-stream` emitting three events 300 ms
//!   apart: proves events reach the HTTP client incrementally (flushed
//!   through Tomcat) instead of after the stream ends.
//! * `/stream/big` (200 KiB) and `/stream/huge` (5 MiB) — larger than the
//!   pooled DIRECT buffer (64 KiB initial, 4 MiB cap); each run bumps a
//!   counter readable at `/stream/count`, proving a DIRECT overflow does not
//!   re-run the handler.
//! * `/stream/slow` (400 ms) and `/stream/stuck` (60 s) — in-flight work for
//!   the graceful-shutdown test.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vespera::axum::body::{Body, Bytes};
use vespera::axum::http::header;
use vespera::axum::response::{IntoResponse, Response};

static BIG_RUNS: AtomicUsize = AtomicUsize::new(0);
static HUGE_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Three server-sent events, 300 ms apart.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/events", tags = ["stream"])]
pub async fn events() -> Response {
    let stream = futures_util::stream::unfold(0u32, |n| async move {
        if n >= 3 {
            return None;
        }
        if n > 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let event = Bytes::from(format!("data: event-{n}\n\n"));
        Some((Ok::<_, std::convert::Infallible>(event), n + 1))
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

/// 200 KiB response (overflows the 64 KiB initial DIRECT buffer).
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/big", tags = ["stream"])]
pub async fn big() -> Vec<u8> {
    BIG_RUNS.fetch_add(1, Ordering::SeqCst);
    vec![b'b'; 200 * 1024]
}

/// 5 MiB response (beyond the 4 MiB DIRECT pool cap: streamed from the stash).
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/huge", tags = ["stream"])]
pub async fn huge() -> Vec<u8> {
    HUGE_RUNS.fetch_add(1, Ordering::SeqCst);
    vec![b'h'; 5 * 1024 * 1024]
}

/// Handler run counts: `"<big>,<huge>"`.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/count", tags = ["stream"])]
pub async fn count() -> String {
    format!(
        "{},{}",
        BIG_RUNS.load(Ordering::SeqCst),
        HUGE_RUNS.load(Ordering::SeqCst)
    )
}

/// Completes after 400 ms.
#[vespera::route(get, path = "/slow", tags = ["stream"])]
pub async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(400)).await;
    "slow"
}

/// Completes after 60 s (only a shutdown ends it earlier).
#[vespera::route(get, path = "/stuck", tags = ["stream"])]
pub async fn stuck() -> &'static str {
    tokio::time::sleep(Duration::from_secs(60)).await;
    "stuck"
}
