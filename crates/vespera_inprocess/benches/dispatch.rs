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
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use axum::{
    Json, Router,
    http::{HeaderMap, HeaderName},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, RequestEnvelope, dispatch_bidirectional_streaming,
    dispatch_from_bytes, dispatch_into, dispatch_into_async_borrowed, dispatch_owned,
    dispatch_streaming_async, dispatch_typed, register_app,
};

// Bench under mimalloc to match the shipped JNI cdylib (which enables mimalloc
// by default).  Without this, the default Windows system heap routes the
// per-request `Vec` allocations these benches stress (input `wire.clone()`,
// response materialisation) through a slow VirtualAlloc commit/decommit path
// for blocks >= ~1 MiB, producing a ~10x large-body "cliff" that no shipped
// build ever pays.  See the `mimalloc` dev-dependency note in Cargo.toml.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// `assemble_wire` with an arbitrary request-header set (used by the
/// request-header-scan bench — the real-world multi-header shape the
/// single-header `assemble_wire` cannot express).
fn assemble_wire_with_headers(
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let header_map: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
        .collect();
    let header = serde_json::json!({
        "v": 1,
        "method": method,
        "path": path,
        "headers": header_map,
    });
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

/// Raw-byte isolation: `dispatch_from_bytes` against `/echo/bytes`,
/// which echoes the request body unchanged.  Comparing this group with
/// `wire_path` (JSON `/echo`) isolates the `serde_json`
/// deserialize+reserialize cost from vespera's pure dispatch/copy
/// overhead at identical body sizes.
fn bench_bytes_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("bytes_path");

    for &body_kb in &[1_usize, 64, 1024] {
        let payload = vec![0xA5u8; body_kb * 1024];
        let wire = assemble_wire(
            "POST",
            "/echo/bytes",
            Some("application/octet-stream"),
            &payload,
        );
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        group.bench_with_input(
            BenchmarkId::new("raw_bytes_dispatch_from_bytes", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
            },
        );
    }

    group.finish();
    drop(runtime);
}

/// Direct-write A/B: `dispatch_from_bytes` (materialises the wire
/// response into a fresh `Vec` per call) vs `dispatch_into` (streams
/// the wire response straight into a caller-owned, preallocated buffer
/// — the JNI `dispatchDirect` path).  Both echo a raw byte body via
/// `/echo/bytes`, so the delta isolates the response `Vec` allocation +
/// final body memcpy that the direct-write path removes.
///
/// The `dispatch_into` buffer is sized exactly once (outside the timed
/// loop) and reused across iterations, mirroring the pooled direct
/// buffer the Java bridge hands in.
fn bench_direct_write_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("direct_write_path");

    // Bodyless GET — the #3 borrowed-input sweet spot. Same-run A/B:
    // `bodyless_owned` clones the wire into a `Vec` (mirrors the JNI
    // `dispatchDirect0` `.to_vec()` copy of the direct buffer), while
    // `bodyless_borrowed` reads the wire in place and builds an empty body,
    // copying nothing. The delta isolates the eliminated input copy.
    {
        let wire = assemble_wire("GET", "/r0", None, &[]);
        let required = {
            let mut probe = vec![0u8; 4096];
            match dispatch_into(wire.clone(), &mut probe, &runtime) {
                DirectWriteResult::Complete(n) | DirectWriteResult::Overflow(n) => n,
            }
        };
        group.bench_function("bodyless_owned_dispatch_into", |b| {
            let mut out = vec![0u8; required];
            b.iter(|| dispatch_into(wire.clone(), &mut out, &runtime));
        });
        group.bench_function("bodyless_borrowed_dispatch_into", |b| {
            let mut out = vec![0u8; required];
            b.iter(|| runtime.block_on(dispatch_into_async_borrowed(&wire, &mut out)));
        });
    }

    for &body_kb in &[64_usize, 1024, 4096] {
        let payload = vec![0xA5u8; body_kb * 1024];
        let wire = assemble_wire(
            "POST",
            "/echo/bytes",
            Some("application/octet-stream"),
            &payload,
        );
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        // Exact response size: one untimed probe with a generous buffer.
        let required = {
            let mut probe = vec![0u8; payload.len() + 4096];
            match dispatch_into(wire.clone(), &mut probe, &runtime) {
                DirectWriteResult::Complete(n) | DirectWriteResult::Overflow(n) => n,
            }
        };

        group.bench_with_input(
            BenchmarkId::new("materialize_dispatch_from_bytes", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("direct_write_dispatch_into", body_kb),
            &body_kb,
            |b, _| {
                let mut out = vec![0u8; required];
                b.iter(|| dispatch_into(wire.clone(), &mut out, &runtime));
            },
        );
    }

    group.finish();
    drop(runtime);
}

/// P2 isolation (within-run A/B): default-app resolution via the
/// lock-free `OnceLock` fast path vs named-app resolution through the
/// lock-free `ArcSwap` load (INP-07).  Identical router, identical wire
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
    // Named-app resolution now goes through the lock-free `ArcSwap` load
    // (INP-07), not the former `RwLock<HashMap>`.
    group.bench_function("named_arcswap_path", |b| {
        b.iter(|| dispatch_from_bytes(wire_named.clone(), &runtime));
    });

    group.finish();
    drop(runtime);
}

/// P2 contention measurement: concurrent `dispatch_from_bytes` from
/// many OS threads against one shared multi-thread runtime.
///
/// `default` resolves through the lock-free `OnceLock` fast path;
/// `named` resolves through the lock-free `ArcSwap` load (INP-07).
/// Both stay lock-free under reader pressure — the residual delta is
/// the `OnceLock` single-atomic-load advantage over the `ArcSwap`
/// load-plus-hash-lookup, which the single-threaded `resolve_path`
/// group cannot isolate.  See `registry_ab` for the RwLock-vs-ArcSwap
/// before/after.
/// Excluded from the CI regression gate (heavily scheduler-dependent);
/// run locally for the numbers.
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
            ("named_arcswap", Some("bench-named")),
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

/// INP-07 before/after A/B: named-app router resolution under
/// concurrent reader pressure — the **previous** `RwLock<HashMap>`
/// registry vs the **current** `ArcSwap<HashMap>` registry, both
/// populated identically and both doing the exact `lookup +
/// Router::clone` the dispatch read path performs.  The synchronization
/// primitive is the only difference, so the delta is the pure
/// lock-vs-lock-free read cost INP-07 buys.
///
/// The single-threaded `resolve_path` group cannot show this — the win
/// is reader *scalability*, which only appears once many threads hammer
/// the shared map (RwLock readers contend on one reader-count cache
/// line; `ArcSwap` shards that away).  Heavily scheduler-dependent;
/// run locally for the numbers.
fn bench_registry_ab(c: &mut Criterion) {
    use arc_swap::ArcSwap;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    let make_map = || {
        let mut m: HashMap<String, Router> = HashMap::new();
        m.insert("bench-named".to_owned(), build_router(100));
        m
    };
    let rwlock: Arc<RwLock<HashMap<String, Router>>> = Arc::new(RwLock::new(make_map()));
    let arcswap: Arc<ArcSwap<HashMap<String, Router>>> =
        Arc::new(ArcSwap::from_pointee(make_map()));

    let mut group = c.benchmark_group("registry_ab");

    for &threads in &[8_usize, 32] {
        // BEFORE — one RwLock read-lock acquisition per resolution.
        let rwlock_b = Arc::clone(&rwlock);
        group.bench_with_input(
            BenchmarkId::new("rwlock_read_before", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let per_thread = usize::try_from(iters)
                        .unwrap_or(usize::MAX)
                        .div_ceil(threads);
                    let start = std::time::Instant::now();
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            let rwlock = Arc::clone(&rwlock_b);
                            scope.spawn(move || {
                                for _ in 0..per_thread {
                                    let guard = rwlock
                                        .read()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    std::hint::black_box(guard.get("bench-named").cloned());
                                }
                            });
                        }
                    });
                    start.elapsed()
                });
            },
        );

        // AFTER — one lock-free `ArcSwap` load per resolution.
        let arcswap_a = Arc::clone(&arcswap);
        group.bench_with_input(
            BenchmarkId::new("arcswap_read_after", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let per_thread = usize::try_from(iters)
                        .unwrap_or(usize::MAX)
                        .div_ceil(threads);
                    let start = std::time::Instant::now();
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            let arcswap = Arc::clone(&arcswap_a);
                            scope.spawn(move || {
                                for _ in 0..per_thread {
                                    std::hint::black_box(
                                        arcswap.load().get("bench-named").cloned(),
                                    );
                                }
                            });
                        }
                    });
                    start.elapsed()
                });
            },
        );
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
                        ControlFlow::Continue(())
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
                            ControlFlow::Continue(())
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
                            ControlFlow::Continue(())
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

/// #2 isolation: the `vespera_jni::dispatchAsync` spawn mechanism.
///
/// Both variants run the dispatch task on a shared multi-thread runtime
/// (the outer `tokio::spawn`, common to both) and differ only in how a
/// panic in the dispatch future is isolated:
///
/// - `double_spawn_pre`: a **second** `tokio::spawn` (panic → `JoinError`),
///   the pre-#2 shape — one extra task allocation + scheduler hop.
/// - `single_spawn_catch_unwind_post`: `FutureExt::catch_unwind` in place,
///   the post-#2 shape — same panic → fallback, no second task.
///
/// The inner future is trivial so the spawn/catch_unwind overhead is the
/// dominant cost and the delta isolates exactly what #2 removes per async
/// dispatch (independent of the dispatch payload size).
fn bench_async_spawn_pattern(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let mut group = c.benchmark_group("async_spawn_pattern");

    group.bench_function("double_spawn_pre", |b| {
        b.iter(|| {
            runtime.block_on(async {
                tokio::spawn(async move {
                    tokio::spawn(async { vec![0u8; 64] })
                        .await
                        .unwrap_or_else(|_| vec![1u8; 16])
                })
                .await
                .unwrap()
            })
        });
    });

    group.bench_function("single_spawn_catch_unwind_post", |b| {
        b.iter(|| {
            runtime.block_on(async {
                tokio::spawn(async move {
                    AssertUnwindSafe(async { vec![0u8; 64] })
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| vec![1u8; 16])
                })
                .await
                .unwrap()
            })
        });
    });

    group.finish();
    drop(runtime);
}

/// Hand-rolled wire-header serde vs `serde_json` (within-run A/B).
///
/// Gates the Oracle-ranked #2 change: replacing `serde_json` on the
/// FIXED-SCHEMA wire header with a hand-rolled parser/writer.  Both arms
/// run in the SAME criterion run (noise-robust, like the
/// `direct_write_path/bodyless_*` group), so the hand vs serde delta is
/// read directly without cross-run drift.
///
/// - `request_parse_*`: full header parse of a realistic small
///   `GET /health`-shaped header (the SmartDispatch DIRECT sweet spot) —
///   `parse_wire_header` (hand) vs `parse_wire_header_serde`.
/// - `response_serialize_*`: slice-serialize of a many-header response
///   (10 single-value + 3-value `set-cookie` + content-type/length) —
///   `write_wire_header_into_slice` (hand) vs the `serde_json` twin.
fn bench_wire_header_serde(c: &mut Criterion) {
    use vespera_inprocess::ResponseMetadata;
    use vespera_inprocess::bench_support::{
        bench_parse_hand, bench_parse_serde, bench_write_hand, bench_write_serde,
    };

    // Request-parse fixture: exactly the JSON object `parse_wire_header`
    // receives (no length prefix) for a small idempotent GET.
    let request_header: &[u8] = br#"{"v":1,"method":"GET","path":"/health","headers":{"accept":"*/*","user-agent":"bench/1.0","host":"localhost:3000"}}"#;

    // Forward-compat fixture: the same small GET plus UNKNOWN header fields
    // (an object with escaped-string values + nesting, and an array). These
    // are ignored by both parsers via the value-skip path — the input shape
    // a newer client / custom FFI caller can legitimately send. Isolates the
    // unknown-value skip cost (escaped-string skip allocation + the recursion
    // depth guard) that the standard `request_header` fixture never exercises.
    let request_header_unknown: &[u8] = br#"{"v":1,"method":"GET","path":"/health","headers":{"accept":"*/*"},"x-meta":{"trace":"a\"b\nc\td","span":"00f0\u00e9","nested":{"k":[1,2,"v\u00e9"]}},"flags":[true,null,42,-3.14e2]}"#;

    // Response-serialize fixture: the realistic many-header response shape
    // (mirrors `handler_many_headers`) plus content-type / content-length.
    let mut resp_headers = HeaderMap::new();
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
        ("content-type", "application/json"),
        ("content-length", "1024"),
    ] {
        resp_headers.insert(
            HeaderName::from_static(name),
            value.parse().expect("static header value"),
        );
    }
    let cookie = HeaderName::from_static("set-cookie");
    resp_headers.append(cookie.clone(), "session=s1; HttpOnly".parse().unwrap());
    resp_headers.append(cookie.clone(), "theme=dark; Path=/".parse().unwrap());
    resp_headers.append(cookie, "lang=en; Path=/".parse().unwrap());
    let metadata = ResponseMetadata::current();

    let mut group = c.benchmark_group("wire_header_serde");

    group.bench_function("request_parse_hand", |b| {
        b.iter(|| bench_parse_hand(std::hint::black_box(request_header)));
    });
    group.bench_function("request_parse_serde", |b| {
        b.iter(|| bench_parse_serde(std::hint::black_box(request_header)));
    });

    // Forward-compat unknown-field skip path (escaped-string skip + depth
    // guard). Standard `request_parse_hand` never enters `skip_value`, so this
    // is where the non-allocating escaped-string skip shows up.
    group.bench_function("request_parse_unknown_hand", |b| {
        b.iter(|| bench_parse_hand(std::hint::black_box(request_header_unknown)));
    });
    group.bench_function("request_parse_unknown_serde", |b| {
        b.iter(|| bench_parse_serde(std::hint::black_box(request_header_unknown)));
    });

    // Size the out buffer once (outside the timed loop) and reuse it,
    // mirroring the pooled direct buffer the JNI bridge hands in.
    let required = bench_write_hand(&mut [0u8; 1024], 200, &resp_headers, &metadata);
    group.bench_function("response_serialize_hand", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_hand(&mut out, 200, &resp_headers, &metadata));
    });
    group.bench_function("response_serialize_serde", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_serde(&mut out, 200, &resp_headers, &metadata));
    });

    group.finish();
}

/// Direct `Request<Body>` construction vs the `http::request::Builder` state
/// machine (within-run A/B).  Both arms build a full request from the same
/// method / path / query / headers / body in the SAME criterion run
/// (noise-robust, like `wire_header_serde`), so the builder-vs-direct delta is
/// read without cross-run drift.  Each arm sums the built request's field byte
/// lengths so neither can be optimised down to a partial build.
///
/// Fixtures span the dispatch hot path's real request shapes: a bodyless `GET`
/// (the DIRECT sweet spot), a `GET` with 3 headers, a small `POST` with
/// `content-type`, and a `POST` with 8 realistic headers.
fn bench_request_build_path(c: &mut Criterion) {
    use vespera_inprocess::bench_support::{bench_build_request_new, bench_build_request_old};

    type Fixture = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static str,
    );
    let fixtures: &[Fixture] = &[
        ("bodyless_get", "GET", "/r0", "", &[], ""),
        (
            "get_3_headers",
            "GET",
            "/r0",
            "",
            &[
                ("accept", "*/*"),
                ("user-agent", "bench/1.0"),
                ("host", "localhost:3000"),
            ],
            "",
        ),
        (
            "post_content_type",
            "POST",
            "/echo",
            "",
            &[("content-type", "application/json")],
            r#"{"body":"x"}"#,
        ),
        (
            "post_8_headers",
            "POST",
            "/echo",
            "",
            &[
                ("content-type", "application/json"),
                ("accept", "*/*"),
                ("user-agent", "bench/1.0"),
                ("host", "localhost:3000"),
                ("authorization", "Bearer abcdef0123456789"),
                ("accept-encoding", "gzip, deflate, br"),
                ("accept-language", "en-US,en;q=0.9"),
                ("x-request-id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y"),
            ],
            r#"{"body":"x"}"#,
        ),
    ];

    let mut group = c.benchmark_group("request_build_ab");
    for &(label, method, path, query, headers, body) in fixtures {
        let body = bytes::Bytes::copy_from_slice(body.as_bytes());
        group.bench_function(BenchmarkId::new("direct_new", label), |b| {
            b.iter(|| bench_build_request_new(method, path, query, headers, body.clone()));
        });
        group.bench_function(BenchmarkId::new("builder_old", label), |b| {
            b.iter(|| bench_build_request_old(method, path, query, headers, body.clone()));
        });
    }
    group.finish();
}

/// Typed-deserialize vs `serde_json::Value` DOM for the 422 validation-error
/// hoist (within-run A/B).  Both arms parse the same framework-generated
/// `{"errors":[{"path","message"}]}` envelope in the SAME criterion run, so
/// the DOM-removal delta is read without cross-run drift.  Each arm sums the
/// hoisted field byte lengths so neither can be optimised to a partial parse.
///
/// Fixtures: a 1-error envelope (typical single-field failure) and a 5-error
/// envelope (form-heavy request) — where the eliminated `Value` map/array/key
/// allocations scale with error count.
fn bench_hoist_422_path(c: &mut Criterion) {
    use vespera_inprocess::bench_support::{bench_hoist_new, bench_hoist_old};

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("content-type"),
        "application/json".parse().expect("static header value"),
    );

    let body_1: &str = r#"{"errors":[{"path":"email","message":"not a valid email"}]}"#;
    let body_5: &str = r#"{"errors":[{"path":"username","message":"length is lower than 3"},{"path":"email","message":"not a valid email"},{"path":"age","message":"greater than 120"},{"path":"bio","message":"length is greater than 256"},{"path":"phone","message":"not a valid phone number"}]}"#;

    let mut group = c.benchmark_group("hoist_422_ab");
    for (label, body) in [("errors_1", body_1), ("errors_5", body_5)] {
        let body = bytes::Bytes::copy_from_slice(body.as_bytes());
        group.bench_function(BenchmarkId::new("typed_new", label), |b| {
            b.iter(|| bench_hoist_new(&headers, &body));
        });
        group.bench_function(BenchmarkId::new("value_old", label), |b| {
            b.iter(|| bench_hoist_old(&headers, &body));
        });
    }
    group.finish();
}

/// Request-header handling cost: a POST carrying a realistic multi-header
/// set (the shape a real browser / reverse-proxy sends) dispatched
/// end-to-end via `dispatch_from_bytes`.  The `wire_path` / `bytes_path`
/// groups send only ONE request header (content-type), so they cannot
/// surface the per-request header-scan cost; this group does, for 1 / 8 /
/// 16 headers, isolating the content-type pre-scan that the dispatch path
/// previously ran separately from the request-build header loop.
fn bench_request_headers_path(c: &mut Criterion) {
    install_bench_app();

    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("request_headers_path");

    // Realistic request headers (browser / proxy shape).  content-type is
    // present (so a POST extractor is satisfied) but sorts into the middle
    // of the JSON object, mirroring how a real header set is scanned.
    let all_headers: &[(&str, &str)] = &[
        ("host", "api.example.com"),
        ("user-agent", "Mozilla/5.0 (bench) Gecko/20100101"),
        ("accept", "application/json, text/plain, */*"),
        ("accept-encoding", "gzip, deflate, br"),
        ("accept-language", "en-US,en;q=0.9"),
        ("content-type", "application/json"),
        (
            "authorization",
            "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        ),
        ("x-request-id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y"),
        ("x-forwarded-for", "203.0.113.7"),
        ("x-forwarded-proto", "https"),
        ("referer", "https://app.example.com/dashboard"),
        ("cookie", "session=abc123; theme=dark; lang=en"),
        ("origin", "https://app.example.com"),
        ("cache-control", "no-cache"),
        ("connection", "keep-alive"),
        ("dnt", "1"),
    ];
    let body = br#"{"body":"x"}"#;

    for &n in &[1_usize, 8, 16] {
        let headers = &all_headers[..n.min(all_headers.len())];
        let wire = assemble_wire_with_headers("POST", "/echo", headers, body);
        group.bench_with_input(BenchmarkId::new("dispatch_from_bytes", n), &n, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }

    group.finish();
    drop(runtime);
}

criterion_group!(
    benches,
    bench_request_headers_path,
    bench_router_path,
    bench_dispatch_path,
    bench_wire_path,
    bench_bytes_path,
    bench_direct_write_path,
    bench_resolve_path,
    bench_contended_path,
    bench_registry_ab,
    bench_headers_path,
    bench_streaming_path,
    bench_async_spawn_pattern,
    bench_wire_header_serde,
    bench_request_build_path,
    bench_hoist_422_path
);
criterion_main!(benches);
