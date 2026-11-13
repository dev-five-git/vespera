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
//!   `bidirectional`), `method`, `route`, `app`,
//!   `status` (the final wire status, including synthesised error statuses),
//!   `duration_ms` (recorded when the dispatch future completes or is
//!   dropped) and `panicked` (`false` unless a panic unwound out of it).
//!   `route` is the axum route **template** that matched (`MatchedPath`,
//!   e.g. `/users/{id}`), never the concrete request path: path segments
//!   can carry secrets (tokens, e-mails) and would make the field
//!   unbounded-cardinality.  It is recorded by a `route_layer` on the
//!   [`TracedApp`] twin of every registered app, so a request no route
//!   matched (`404` fallback, rejected before routing) leaves it unset.
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
//! `Span::current()` lookup at the record points (and when picking the
//! router), plus the route-recording layer's service hop.  Without an
//! interested subscriber the registry hands out the uninstrumented router,
//! so the only extra work is that `Span::current()` check.

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
        route = Empty,
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

/// Record the parsed request line (method + app, never the concrete path)
/// onto the current dispatch span.
pub fn record_request(method: &str, app: Option<&str>) {
    if let Some(span) = current_dispatch_span() {
        let app = app
            .filter(|app| !app.is_empty())
            .unwrap_or(crate::DEFAULT_APP_NAME);
        span.record("method", method);
        span.record("app", app);
    }
}

/// A registered app as the registry stores it with the feature on: the
/// factory's router, plus a twin whose routes carry [`RecordRouteLayer`]
/// (built once per registration, never per request; both share the
/// handlers).  [`TracedApp::router`] hands out the twin only while a
/// `vespera.dispatch` span is recording, so a build with the feature on but
/// no interested subscriber dispatches the exact uninstrumented router.
#[derive(Clone)]
pub struct TracedApp {
    plain: crate::Router,
    traced: crate::Router,
}

impl TracedApp {
    /// Pair `router` with its route-recording twin (a router without routes
    /// is used as is: axum rejects a `route_layer` on it).
    pub fn new(router: crate::Router) -> Self {
        let traced = if router.has_routes() {
            router.clone().route_layer(RecordRouteLayer)
        } else {
            router.clone()
        };
        Self {
            plain: router,
            traced,
        }
    }

    /// The router to dispatch: the twin inside a recording dispatch span.
    pub fn router(&self) -> crate::Router {
        if current_dispatch_span().is_some() {
            self.traced.clone()
        } else {
            self.plain.clone()
        }
    }
}

/// `route_layer` that records the matched route template.
#[derive(Clone, Copy)]
pub struct RecordRouteLayer;

impl<S> tower::Layer<S> for RecordRouteLayer {
    type Service = RecordRoute<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RecordRoute(inner)
    }
}

/// Service half of [`RecordRouteLayer`]: runs after axum routed the
/// request (so `MatchedPath` is set) and inside the dispatch span's poll.
#[derive(Clone)]
pub struct RecordRoute<S>(S);

impl<S: tower::Service<axum::extract::Request>> tower::Service<axum::extract::Request>
    for RecordRoute<S>
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: axum::extract::Request) -> Self::Future {
        if let Some(route) = request.extensions().get::<axum::extract::MatchedPath>() {
            record_route(route.as_str());
        }
        self.0.call(request)
    }
}

/// Record the matched route template onto the current dispatch span.
fn record_route(route: &str) {
    if let Some(span) = current_dispatch_span() {
        span.record("route", route);
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
