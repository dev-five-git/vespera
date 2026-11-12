//! `tracing` instrumentation (cargo feature `tracing`): the per-dispatch
//! `vespera.dispatch` span and the error / panic events, captured with a
//! `Registry` + a tiny collecting `Layer`.
//!
//! Own test binary: it pins a small ingress cap (process-global `OnceLock`)
//! and registers its own default app.
#![cfg(feature = "tracing")]

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use axum::routing::get;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, Router, dispatch_bidirectional_streaming, dispatch_from_bytes,
    dispatch_from_bytes_async, dispatch_into, dispatch_streaming_async, error_wire, register_app,
    register_app_named, set_max_request_bytes,
};

const CAP: usize = 4096;

// ── Capture layer ─────────────────────────────────────────────────────

type Fields = BTreeMap<String, String>;

#[derive(Debug)]
struct SpanRec {
    name: &'static str,
    fields: Fields,
}

#[derive(Debug)]
struct EventRec {
    level: Level,
    fields: Fields,
    /// Name of the span the event belongs to (explicit or contextual parent).
    span: Option<&'static str>,
}

#[derive(Default)]
struct State {
    spans: Vec<SpanRec>,
    by_id: BTreeMap<u64, usize>,
    events: Vec<EventRec>,
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<State>>);

struct Visitor<'a>(&'a mut Fields);

impl Visit for Visitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        let mut fields = Fields::new();
        attrs.record(&mut Visitor(&mut fields));
        let mut state = self.0.lock().unwrap();
        let index = state.spans.len();
        state.spans.push(SpanRec {
            name: attrs.metadata().name(),
            fields,
        });
        state.by_id.insert(id.into_u64(), index);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let index = state.by_id[&id.into_u64()];
        values.record(&mut Visitor(&mut state.spans[index].fields));
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::new();
        event.record(&mut Visitor(&mut fields));
        let span = ctx.event_span(event).map(|span| span.name());
        self.0.lock().unwrap().events.push(EventRec {
            level: *event.metadata().level(),
            fields,
            span,
        });
    }
}

impl Capture {
    /// Run `f` with this layer as the thread's subscriber.
    fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        let subscriber = tracing_subscriber::registry().with(self.clone());
        tracing::subscriber::with_default(subscriber, f)
    }

    /// The single `vespera.dispatch` span recorded so far.
    fn dispatch_span(&self) -> Fields {
        let state = self.0.lock().unwrap();
        let mut spans = state.spans.iter().filter(|s| s.name == "vespera.dispatch");
        let span = spans.next().expect("a vespera.dispatch span");
        assert!(spans.next().is_none(), "exactly one dispatch span");
        span.fields.clone()
    }

    fn events(&self) -> Vec<(Level, String, Option<&'static str>)> {
        let state = self.0.lock().unwrap();
        state
            .events
            .iter()
            .map(|e| (e.level, e.fields["message"].clone(), e.span))
            .collect()
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────

async fn panics() -> &'static str {
    panic!("handler boom")
}

fn app() -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }).post(|| async { "posted" }))
        .route("/panic", get(panics))
}

fn setup() {
    let _ = set_max_request_bytes(CAP);
    register_app(app);
    register_app_named("admin", app);
}

fn wire(header: &str, body: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(header.as_bytes());
    wire.extend_from_slice(body);
    wire
}

fn get_wire(path: &str) -> Vec<u8> {
    wire(&format!(r#"{{"v":1,"method":"GET","path":"{path}"}}"#), b"")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn status_of(wire: &[u8]) -> u16 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    u16::try_from(header["status"].as_u64().unwrap()).unwrap()
}

fn field<'a>(fields: &'a Fields, name: &str) -> &'a str {
    fields.get(name).map_or("<unset>", String::as_str)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[test]
fn success_span_records_request_status_and_duration() {
    setup();
    let capture = Capture::default();
    let resp = capture.run(|| {
        dispatch_from_bytes(
            wire(
                r#"{"v":1,"method":"GET","path":"/ok","query":"token=secret"}"#,
                b"",
            ),
            &runtime(),
        )
    });
    assert_eq!(status_of(&resp), 200);
    let span = capture.dispatch_span();
    assert_eq!(field(&span, "mode"), "bytes");
    assert_eq!(field(&span, "method"), "GET");
    assert_eq!(field(&span, "path"), "/ok");
    assert_eq!(field(&span, "app"), "_default");
    assert_eq!(field(&span, "status"), "200");
    assert_eq!(field(&span, "panicked"), "false");
    assert!(field(&span, "duration_ms").parse::<f64>().unwrap() >= 0.0);
    assert!(
        !format!("{span:?}").contains("secret"),
        "query leaked: {span:?}"
    );
    assert!(capture.events().is_empty(), "{:?}", capture.events());
}

#[test]
fn path_query_suffix_is_never_recorded_and_named_app_is() {
    setup();
    let capture = Capture::default();
    capture.run(|| {
        dispatch_from_bytes(
            wire(
                r#"{"v":1,"method":"GET","path":"/ok?token=secret","app":"admin"}"#,
                b"",
            ),
            &runtime(),
        )
    });
    let span = capture.dispatch_span();
    assert_eq!(field(&span, "path"), "/ok");
    assert_eq!(field(&span, "app"), "admin");
}

#[test]
fn spawned_buffered_dispatch_is_async_mode() {
    setup();
    let capture = Capture::default();
    let resp = capture.run(|| {
        runtime()
            .block_on(async { tokio::spawn(dispatch_from_bytes_async(get_wire("/ok"))).await })
            .unwrap()
    });
    assert_eq!(status_of(&resp), 200);
    assert_eq!(field(&capture.dispatch_span(), "mode"), "async");
}

#[test]
fn panic_marks_span_and_emits_error_event() {
    setup();
    let capture = Capture::default();
    let caught = capture
        .run(|| std::panic::catch_unwind(|| dispatch_from_bytes(get_wire("/panic"), &runtime())));
    assert!(caught.is_err(), "the handler panic propagates to the host");
    let span = capture.dispatch_span();
    assert_eq!(field(&span, "panicked"), "true");
    assert_eq!(field(&span, "status"), "500");
    assert_eq!(field(&span, "path"), "/panic");
    assert!(field(&span, "duration_ms").parse::<f64>().is_ok());
    assert_eq!(
        capture.events(),
        [(
            Level::ERROR,
            "vespera dispatch panicked".to_owned(),
            Some("vespera.dispatch")
        )]
    );
}

#[test]
fn oversized_request_is_413_with_warn_event() {
    setup();
    let capture = Capture::default();
    let resp = capture.run(|| {
        dispatch_from_bytes(
            wire(r#"{"v":1,"method":"POST","path":"/ok"}"#, &[b'x'; CAP]),
            &runtime(),
        )
    });
    assert_eq!(status_of(&resp), 413);
    assert_eq!(field(&capture.dispatch_span(), "status"), "413");
    assert_eq!(
        capture.events(),
        [(
            Level::WARN,
            "vespera dispatch: request exceeds size limit".to_owned(),
            Some("vespera.dispatch")
        )]
    );
}

#[test]
fn malformed_wire_is_400_with_warn_event() {
    setup();
    let capture = Capture::default();
    let resp = capture.run(|| dispatch_from_bytes(vec![0, 0], &runtime()));
    assert_eq!(status_of(&resp), 400);
    let span = capture.dispatch_span();
    assert_eq!(field(&span, "status"), "400");
    assert_eq!(field(&span, "method"), "<unset>", "nothing parsed");
    assert_eq!(
        capture.events(),
        [(
            Level::WARN,
            "vespera dispatch: malformed request".to_owned(),
            Some("vespera.dispatch")
        )]
    );
}

#[test]
fn unknown_app_is_a_rejected_request() {
    setup();
    let capture = Capture::default();
    let resp = capture.run(|| {
        dispatch_from_bytes(
            wire(r#"{"v":1,"method":"GET","path":"/ok","app":"nope"}"#, b""),
            &runtime(),
        )
    });
    assert_eq!(status_of(&resp), 404);
    let span = capture.dispatch_span();
    assert_eq!(field(&span, "app"), "nope");
    assert_eq!(field(&span, "status"), "404");
    assert_eq!(
        capture.events()[0].1,
        "vespera dispatch: request rejected".to_owned()
    );
}

#[test]
fn direct_streaming_and_bidirectional_modes() {
    setup();
    let capture = Capture::default();
    capture.run(|| {
        let rt = runtime();
        let mut out = vec![0u8; 1024];
        let written = dispatch_into(get_wire("/ok"), &mut out, &rt);
        assert!(matches!(written, DirectWriteResult::Complete(_)));
        rt.block_on(dispatch_streaming_async(get_wire("/ok"), |_| {
            ControlFlow::Continue(())
        }));
        rt.block_on(dispatch_bidirectional_streaming(
            get_wire("/ok"),
            || RequestChunk::End,
            |_| ControlFlow::Continue(()),
        ));
    });
    let state = capture.0.lock().unwrap();
    let modes: Vec<_> = state
        .spans
        .iter()
        .map(|s| (s.fields["mode"].as_str(), s.fields["status"].as_str()))
        .collect();
    assert_eq!(
        modes,
        [
            ("direct", "200"),
            ("streaming", "200"),
            ("bidirectional", "200")
        ]
    );
}

#[test]
fn error_wire_events_by_status_outside_a_dispatch() {
    let capture = Capture::default();
    capture.run(|| {
        // A host span with a `status` field is never written to.
        let host = tracing::info_span!("host", status = tracing::field::Empty);
        let _entered = host.enter();
        for status in [431, 503, 504, 405, 500] {
            let _ = error_wire(status, "detail");
        }
    });
    let messages: Vec<_> = capture
        .events()
        .into_iter()
        .map(|(level, message, _)| (level, message))
        .collect();
    assert_eq!(
        messages,
        [
            (
                Level::WARN,
                "vespera dispatch: too many request headers".to_owned()
            ),
            (
                Level::WARN,
                "vespera dispatch: runtime unavailable".to_owned()
            ),
            (
                Level::WARN,
                "vespera dispatch: deadline exceeded".to_owned()
            ),
            (Level::WARN, "vespera dispatch: request rejected".to_owned()),
            (Level::ERROR, "vespera dispatch: internal error".to_owned()),
        ]
    );
    let state = capture.0.lock().unwrap();
    assert!(!state.spans[0].fields.contains_key("status"));
}

#[test]
fn no_subscriber_dispatches_uninstrumented() {
    setup();
    assert_eq!(
        status_of(&dispatch_from_bytes(get_wire("/ok"), &runtime())),
        200
    );
}
