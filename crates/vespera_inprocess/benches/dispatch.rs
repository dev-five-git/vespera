//! Criterion benchmarks for the in-process dispatch surface.
//!
//! Five groups:
//!
//! - `router_path`: `Router::clone()` of a pre-built router  (post-P1)
//!   vs rebuilding the router from a factory closure        (pre-P1, simulated).
//! - `dispatch_path`: `dispatch_owned(router, env)`          (post-P2)
//!   vs `dispatch_typed(router, &env)` which clones internally (pre-P2).
//! - `wire_path`: end-to-end `dispatch_from_bytes` — wire-format
//!   round-trip including header JSON parse + body byte handling.
//! - `headers_path`: `dispatch_from_bytes` against a route that sets
//!   many response headers (incl. multi-value `set-cookie`) —
//!   isolates `collect_header_map` + wire header serialisation cost.
//! - `streaming_path`: `dispatch_streaming_async` (response
//!   streaming) and `dispatch_bidirectional_streaming` (request +
//!   response streaming through the mpsc channel + spawn_blocking
//!   producer) — gates the chunk-size / channel-capacity work. Also
//!   includes a no-body-poll route to isolate lazy request-pull setup.
//!
//! Scaling axes:
//! - `route_count`: 10 / 100 / 500 routes (Router-build dominance).
//! - `body_kb`: 1 / 64 / 1024 KB request bodies (body-clone dominance).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::{
    Json, Router,
    http::{HeaderMap, HeaderName},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use vespera_inprocess::{
    RequestChunk, RequestEnvelope, dispatch_bidirectional_streaming, dispatch_from_bytes,
    dispatch_owned, dispatch_streaming_async, dispatch_typed, register_app,
};

// ── Test fixtures ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct Echo {
    body: String,
}

async fn handler_get() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn handler_echo(Json(payload): Json<Echo>) -> Json<Echo> {
    Json(payload)
}

/// Echo raw request-body bytes back — used by the streaming benches
/// so request chunks flow through the handler unchanged.
async fn handler_echo_bytes(body: bytes::Bytes) -> bytes::Bytes {
    body
}

/// Return without polling the request body. This isolates the cost of
/// bidirectional request-pull setup for handlers that do not need the
/// body at all.
async fn handler_discard_body() -> &'static str {
    "ok"
}

/// Respond with a realistic header set: 10 single-value headers plus
/// a 3-value `set-cookie` — exercises `collect_header_map`'s Vacant
/// and Occupied paths and the wire header JSON serialisation.
async fn handler_many_headers() -> Response {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("etag", "\"abc123def456\""),
        ("vary", "accept-encoding"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("x-request-id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y"),
        ("x-trace-id", "4bf92f3577b34da6a3ce929d0e0e4736"),
        ("access-control-allow-origin", "*"),
        ("strict-transport-security", "max-age=63072000"),
        ("content-language", "en"),
    ] {
        headers.insert(
            HeaderName::from_static(name),
            value.parse().expect("static header value"),
        );
    }
    let cookie = HeaderName::from_static("set-cookie");
    headers.append(cookie.clone(), "session=s1; HttpOnly".parse().unwrap());
    headers.append(cookie.clone(), "theme=dark; Path=/".parse().unwrap());
    headers.append(cookie, "lang=en; Path=/".parse().unwrap());
    (headers, "ok").into_response()
}

/// Build a router with `n_routes` distinct GET endpoints plus one
/// `POST /echo` that echoes the request body.
fn build_router(n_routes: usize) -> Router {
    let mut router = Router::new()
        .route("/echo", post(handler_echo))
        .route("/echo/bytes", post(handler_echo_bytes))
        .route("/discard", post(handler_discard_body))
        .route("/headers", get(handler_many_headers));
    for i in 0..n_routes {
        let path = format!("/r{i}");
        router = router.route(&path, get(handler_get));
    }
    router
}

/// Owned `RequestEnvelope` for the direct-API benches.
fn make_envelope(body_kb: usize) -> RequestEnvelope {
    let body_str = "x".repeat(body_kb * 1024);
    let mut headers = HashMap::new();
    headers.insert("content-type".to_owned(), "application/json".to_owned());
    RequestEnvelope {
        method: "POST".to_owned(),
        path: "/echo".to_owned(),
        query: String::new(),
        headers,
        body: serde_json::to_string(&Echo { body: body_str }).unwrap(),
    }
}

/// Assemble `[u32 BE header_len | header JSON | body]` wire bytes.
fn assemble_wire(method: &str, path: &str, content_type: Option<&str>, body: &[u8]) -> Vec<u8> {
    assemble_wire_for_app(method, path, content_type, None, body)
}

/// `assemble_wire` with an optional `"app"` wire-header field.
fn assemble_wire_for_app(
    method: &str,
    path: &str,
    content_type: Option<&str>,
    app: Option<&str>,
    body: &[u8],
) -> Vec<u8> {
    let mut header = content_type.map_or_else(
        || serde_json::json!({ "v": 1, "method": method, "path": path }),
        |ct| {
            serde_json::json!({
                "v": 1,
                "method": method,
                "path": path,
                "headers": {"content-type": ct},
            })
        },
    );
    if let Some(app) = app {
        header["app"] = serde_json::Value::String(app.to_owned());
    }
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let header_len = u32::try_from(header_bytes.len()).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

/// Wire-format request payload for the `dispatch_from_bytes` bench.
fn make_wire_request(body_kb: usize) -> Vec<u8> {
    let body_str = serde_json::to_string(&Echo {
        body: "x".repeat(body_kb * 1024),
    })
    .unwrap();
    assemble_wire(
        "POST",
        "/echo",
        Some("application/json"),
        body_str.as_bytes(),
    )
}

/// Register the shared bench app exactly once per process.
fn install_bench_app() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| register_app(|| build_router(100)));
}

// ── Benchmarks ───────────────────────────────────────────────────────

/// P1 isolation: cached `Router::clone()` vs factory rebuild per call.
fn bench_router_path(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let envelope_template = make_envelope(1); // 1 KB body, fixed
    let mut group = c.benchmark_group("router_path");

    for &n_routes in &[10_usize, 100, 500] {
        let cached = build_router(n_routes);

        group.bench_with_input(
            BenchmarkId::new("cached_clone_post_P1", n_routes),
            &n_routes,
            |b, _| {
                b.iter(|| {
                    let router = cached.clone();
                    runtime.block_on(dispatch_owned(router, envelope_template.clone()))
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("factory_rebuild_pre_P1", n_routes),
            &n_routes,
            |b, &n| {
                b.iter(|| {
                    let router = build_router(n);
                    runtime.block_on(dispatch_owned(router, envelope_template.clone()))
                });
            },
        );
    }

    group.finish();
}

/// P2 isolation: `dispatch_owned` (envelope moved) vs `dispatch_typed`
/// (envelope borrowed → cloned internally).
fn bench_dispatch_path(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let cached = build_router(20);
    let mut group = c.benchmark_group("dispatch_path");

    for &body_kb in &[1_usize, 64, 1024] {
        let template = make_envelope(body_kb);
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        group.bench_with_input(
            BenchmarkId::new("owned_post_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| runtime.block_on(dispatch_owned(cached.clone(), template.clone())));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("borrowed_pre_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| runtime.block_on(dispatch_typed(cached.clone(), &template)));
            },
        );
    }

    group.finish();
}

/// End-to-end binary-wire flow: encoded request bytes → decoded
/// response bytes via the registered app.  Measures the realistic FFI
/// cost the JNI bridge pays.
fn bench_wire_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("wire_path");

    for &body_kb in &[1_usize, 64, 1024] {
        let wire = make_wire_request(body_kb);
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        group.bench_with_input(
            BenchmarkId::new("dispatch_from_bytes", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
            },
        );
    }

    group.finish();
    drop(runtime);
}

/// P2 isolation (within-run A/B): default-app resolution via the
/// lock-free `OnceLock` fast path vs named-app resolution through the
/// `RwLock<HashMap>` slow path.  Identical router, identical wire
/// request shape — the only difference is the `"app"` header field.
fn bench_resolve_path(c: &mut Criterion) {
    static INIT_NAMED: std::sync::Once = std::sync::Once::new();

    install_bench_app();
    INIT_NAMED
        .call_once(|| vespera_inprocess::register_app_named("bench-named", || build_router(100)));

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("resolve_path");

    let wire_default = assemble_wire_for_app("GET", "/r0", None, None, &[]);
    group.bench_function("default_oncelock_fast_path", |b| {
        b.iter(|| dispatch_from_bytes(wire_default.clone(), &runtime));
    });

    let wire_named = assemble_wire_for_app("GET", "/r0", None, Some("bench-named"), &[]);
    group.bench_function("named_rwlock_slow_path", |b| {
        b.iter(|| dispatch_from_bytes(wire_named.clone(), &runtime));
    });

    group.finish();
    drop(runtime);
}

/// P2 contention measurement: concurrent `dispatch_from_bytes` from
/// many OS threads against one shared multi-thread runtime.
///
/// `default` resolves through the lock-free `OnceLock` fast path;
/// `named` goes through the `RwLock<HashMap>`.  Under reader pressure
/// the RwLock path can park threads — the delta between the two
/// captures exactly what the single-threaded `resolve_path` group
/// cannot.  Excluded from the CI regression gate (heavily
/// scheduler-dependent); run locally for the numbers.
fn bench_contended_path(c: &mut Criterion) {
    static INIT_NAMED: std::sync::Once = std::sync::Once::new();

    install_bench_app();
    INIT_NAMED
        .call_once(|| vespera_inprocess::register_app_named("bench-named", || build_router(100)));

    let runtime = std::sync::Arc::new(Runtime::new().expect("tokio runtime"));
    let mut group = c.benchmark_group("contended_path");

    for &threads in &[8_usize, 32] {
        for (label, app) in [
            ("default_oncelock", None),
            ("named_rwlock", Some("bench-named")),
        ] {
            let wire = assemble_wire_for_app("GET", "/r0", None, app, &[]);
            group.bench_with_input(BenchmarkId::new(label, threads), &threads, |b, &threads| {
                b.iter_custom(|iters| {
                    let per_thread = usize::try_from(iters)
                        .unwrap_or(usize::MAX)
                        .div_ceil(threads);
                    let start = std::time::Instant::now();
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            let wire = wire.clone();
                            let runtime = std::sync::Arc::clone(&runtime);
                            scope.spawn(move || {
                                for _ in 0..per_thread {
                                    std::hint::black_box(dispatch_from_bytes(
                                        wire.clone(),
                                        &runtime,
                                    ));
                                }
                            });
                        }
                    });
                    start.elapsed()
                });
            });
        }
    }

    group.finish();
}

/// P4 isolation: response with 10 single-value headers + 3-value
/// `set-cookie` — dominated by `collect_header_map` allocations and
/// wire header JSON serialisation rather than body handling.
fn bench_headers_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let wire = assemble_wire("GET", "/headers", None, &[]);
    let mut group = c.benchmark_group("headers_path");

    group.bench_function("many_headers_roundtrip", |b| {
        b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
    });

    group.finish();
    drop(runtime);
}

/// P1/P3 isolation: streaming dispatch throughput.
///
/// - `response_streaming`: full body in the request, response drained
///   through the `on_chunk` callback.
/// - `bidirectional`: request body fed through `pull_chunk` in
///   [`vespera_inprocess::DEFAULT_STREAMING_CHUNK_BYTES`] pieces
///   (mirrors the JNI `InputStream` reader), response drained through
///   `on_chunk` — exercises the bounded mpsc channel and the
///   `spawn_blocking` producer.
fn bench_streaming_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("streaming_path");

    for &body_kb in &[64_usize, 1024] {
        let payload = vec![0xA5u8; body_kb * 1024];
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        let wire = assemble_wire(
            "POST",
            "/echo/bytes",
            Some("application/octet-stream"),
            &payload,
        );
        group.bench_with_input(
            BenchmarkId::new("response_streaming", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    let mut sink = 0usize;
                    runtime.block_on(dispatch_streaming_async(wire.clone(), |chunk| {
                        sink += chunk.len();
                    }));
                    sink
                });
            },
        );

        let header_only =
            assemble_wire("POST", "/echo/bytes", Some("application/octet-stream"), &[]);
        let pull_chunk_size = vespera_inprocess::DEFAULT_STREAMING_CHUNK_BYTES;
        let request_chunks: Vec<Vec<u8>> = payload
            .chunks(pull_chunk_size)
            .map(<[u8]>::to_vec)
            .collect();
        group.bench_with_input(
            BenchmarkId::new("bidirectional", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    let chunks_iter = Mutex::new(request_chunks.clone().into_iter());
                    let pull = move || -> RequestChunk {
                        chunks_iter
                            .lock()
                            .unwrap()
                            .next()
                            .map_or(RequestChunk::End, RequestChunk::Data)
                    };
                    let mut sink = 0usize;
                    runtime.block_on(dispatch_bidirectional_streaming(
                        header_only.clone(),
                        pull,
                        |chunk| {
                            sink += chunk.len();
                        },
                    ));
                    sink
                });
            },
        );

        let discard_header_only =
            assemble_wire("POST", "/discard", Some("application/octet-stream"), &[]);
        group.bench_with_input(
            BenchmarkId::new("bidirectional_no_body_poll", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    let remaining = Mutex::new(body_kb * 1024);
                    let pull = move || -> RequestChunk {
                        let mut remaining = remaining.lock().unwrap();
                        if *remaining == 0 {
                            return RequestChunk::End;
                        }
                        let len = (*remaining).min(pull_chunk_size);
                        *remaining -= len;
                        RequestChunk::Data(vec![0xA5u8; len])
                    };
                    let mut sink = 0usize;
                    runtime.block_on(dispatch_bidirectional_streaming(
                        discard_header_only.clone(),
                        pull,
                        |chunk| {
                            sink += chunk.len();
                        },
                    ));
                    sink
                });
            },
        );
    }

    group.finish();
    drop(runtime);
}

criterion_group!(
    benches,
    bench_router_path,
    bench_dispatch_path,
    bench_wire_path,
    bench_resolve_path,
    bench_contended_path,
    bench_headers_path,
    bench_streaming_path
);
criterion_main!(benches);
