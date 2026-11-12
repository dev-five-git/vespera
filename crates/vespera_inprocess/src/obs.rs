//! Optional `tracing` instrumentation (cargo feature `tracing`, off by
//! default).
//!
//! The whole module is compiled only with the feature on; every call site is
//! `#[cfg(feature = "tracing")]` (or the `traced!` macro in `lib.rs`, which
//! expands to the bare body when the feature is off), so a default build
//! carries no instrumentation code, no `tracing` dependency and no extra
//! work on the dispatch hot path.
//!
//! # What is emitted
//!
//! * One `vespera.dispatch` **span** per wire dispatch (INFO level), opened
//!   by the entry point and entered on every poll of the dispatch future.
//!   Fields: `mode` (`bytes` / `async` / `direct` / `streaming` /
//!   `bidirectional`), `method`, `path` (never the query string), `app`,
//!   `status` (the final wire status, including synthesised error statuses),
//!   `duration_ms` (recorded when the dispatch future completes or is
//!   dropped) and `panicked` (`false` unless a panic unwound out of it).
//! * A WARN / ERROR **event** for every synthesised error response
//!   ([`error_response`], hooked into `wire::error_wire`): malformed wire /
//!   request (`400`), size limit (`413`), too many headers (`431`), runtime
//!   shut down / overloaded (`503`), deadline (`504`), other `4xx` (WARN)
//!   and engine `5xx` (ERROR).  Handler-produced responses never emit an
//!   event — only the span's `status` field.
//! * An ERROR event (parented to the span) when a panic unwinds out of the
//!   dispatch future, which also sets `panicked = true` and `status = 500`.
//!
//! Cost with the feature on but no interested subscriber: the span macro's
//! cached callsite-interest check, after which the future is awaited
//! unwrapped.  With a subscriber: one span (no field allocations — every
//! value is a `&str` / integer / `f64`), one `Instant::now()` pair, and a
//! `Span::current()` lookup at the two record points.

use std::future::Future;
use std::time::Instant;

use tracing::Span;
use tracing::field::Empty;

/// Span name of the per-dispatch span.
pub const SPAN_NAME: &str = "vespera.dispatch";

/// `mode` of the buffered wire entry point: `async` when it is polled as a
/// spawned Tokio task (the JNI `dispatchAsync` path), `bytes` when it is
/// driven by `block_on` (the JNI `dispatchBytes` / `dispatch_from_bytes`
/// path).
pub fn buffered_mode() -> &'static str {
    if tokio::task::try_id().is_some() {
        "async"
    } else {
        "bytes"
    }
}

/// Run `fut` inside a fresh `vespera.dispatch` span (see the module docs).
pub async fn instrument<F: Future>(mode: &'static str, fut: F) -> F::Output {
    let span = tracing::info_span!(
        "vespera.dispatch",
        mode,
        method = Empty,
        path = Empty,
        app = Empty,
        status = Empty,
        duration_ms = Empty,
        panicked = false,
    );
    if span.is_disabled() {
        return fut.await;
    }
    let _timer = Timer {
        span: &span,
        start: Instant::now(),
    };
    let mut fut = std::pin::pin!(fut);
    std::future::poll_fn(|cx| {
        let _entered = span.enter();
        let _panic = PanicGuard(&span);
        fut.as_mut().poll(cx)
    })
    .await
}

/// Records `duration_ms` when the dispatch future completes, is dropped
/// (cancelled) or unwinds.
struct Timer<'a> {
    span: &'a Span,
    start: Instant,
}

impl Drop for Timer<'_> {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_secs_f64() * 1000.0;
        self.span.record("duration_ms", ms);
    }
}

/// Alive across exactly one poll of the dispatch future: dropped while the
/// thread is panicking means the panic unwound out of that poll.  Checked in
/// the poll itself (not on the future's drop) because an executor such as
/// `tokio::spawn` catches the panic first and drops the future afterwards,
/// when `std::thread::panicking()` is already `false` again.
struct PanicGuard<'a>(&'a Span);

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.record("panicked", true);
            self.0.record("status", 500u16);
            tracing::error!(parent: self.0, "vespera dispatch panicked");
        }
    }
}

/// The current span, if it is a `vespera.dispatch` span (a host span that
/// happens to have a `status` field is never written to).
fn current_dispatch_span() -> Option<Span> {
    let span = Span::current();
    span.metadata()
        .is_some_and(|meta| meta.name() == SPAN_NAME)
        .then_some(span)
}

/// Record the parsed request line onto the current dispatch span.
pub fn record_request(method: &str, path: &str, app: Option<&str>) {
    if let Some(span) = current_dispatch_span() {
        // Defensive: a wire `path` carrying its own `?query` must not leak
        // it (the query is a separate wire field and is never recorded).
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        let app = app
            .filter(|app| !app.is_empty())
            .unwrap_or(crate::DEFAULT_APP_NAME);
        span.record("method", method);
        span.record("path", path);
        span.record("app", app);
    }
}

/// Record the response status onto the current dispatch span.
pub fn record_status(status: u16) {
    if let Some(span) = current_dispatch_span() {
        span.record("status", status);
    }
}

/// Hook for every synthesised error response (`wire::error_wire`): record
/// its status on the current dispatch span and emit the matching event.
pub fn error_response(status: u16, detail: &str) {
    record_status(status);
    match status {
        400 => tracing::warn!(status, detail, "vespera dispatch: malformed request"),
        413 => tracing::warn!(
            status,
            detail,
            "vespera dispatch: request exceeds size limit"
        ),
        431 => tracing::warn!(status, detail, "vespera dispatch: too many request headers"),
        503 => tracing::warn!(status, detail, "vespera dispatch: runtime unavailable"),
        504 => tracing::warn!(status, detail, "vespera dispatch: deadline exceeded"),
        0..500 => tracing::warn!(status, detail, "vespera dispatch: request rejected"),
        _ => tracing::error!(status, detail, "vespera dispatch: internal error"),
    }
}
