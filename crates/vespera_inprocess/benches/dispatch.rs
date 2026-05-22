//! Criterion benchmarks quantifying the performance review patches.
//!
//! Each benchmark group compares **two paths** that are both reachable
//! from the *current* code base, so a single `cargo bench` run produces
//! the before/after comparison without git tricks:
//!
//! - `router_path`: `Router::clone()` of a pre-built router  (post-P1)
//!   vs rebuilding the router from a factory closure        (pre-P1, simulated).
//! - `dispatch_path`: `dispatch_owned(router, env)`          (post-P2)
//!   vs `dispatch(router, &env)` which clones internally     (pre-P2).
//! - `full_flow`: realistic JNI flow `dispatch_from_json`-style — parse +
//!   cached router + owned dispatch (post-P1+P2) vs parse + per-call
//!   build + borrowed dispatch (pre-P1+P2).
//!
//! Scaling axes:
//! - `route_count`: 10 / 100 / 500 routes (Router-build dominance).
//! - `body_kb`: 1 / 64 / 1024 KB request bodies (body-clone dominance).

use std::collections::HashMap;

use axum::{
    Json, Router,
    routing::{get, post},
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use vespera_inprocess::{RequestEnvelope, dispatch, dispatch_owned, dispatch_typed, parse_request};

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

/// Build a router with `n_routes` distinct GET endpoints plus one
/// `POST /echo` that echoes the request body.  This simulates the
/// `vespera!()` macro-expanded `Router::new().route(...).route(...)...`
/// chain that runs inside the user's `create_app()`.
fn build_router(n_routes: usize) -> Router {
    let mut router = Router::new().route("/echo", post(handler_echo));
    for i in 0..n_routes {
        let path = format!("/r{i}");
        router = router.route(&path, get(handler_get));
    }
    router
}

/// JSON-encoded `RequestEnvelope` whose body is `body_kb * 1024` bytes
/// of valid UTF-8 (so we measure the realistic clone/move cost without
/// triggering the lossy decode path).
fn make_envelope_json(body_kb: usize) -> String {
    let body_str = "x".repeat(body_kb * 1024);
    let envelope = serde_json::json!({
        "method": "POST",
        "path": "/echo",
        "query": "",
        "headers": { "content-type": "application/json" },
        "body": serde_json::to_string(&Echo { body: body_str }).unwrap(),
    });
    envelope.to_string()
}

/// Owned `RequestEnvelope` mirror of `make_envelope_json` for the
/// dispatch-only benches that skip the JSON parse step.
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

// ── Naive (pre-patch) reference paths ────────────────────────────────

/// Simulates the pre-patch `dispatch_from_json`:
///   factory() per call  +  dispatch with borrowed envelope (internal clone).
fn naive_dispatch_from_json(
    input: &str,
    runtime: &Runtime,
    factory: &dyn Fn() -> Router,
) -> String {
    let envelope = parse_request(input).expect("valid envelope");
    let router = factory(); // pre-P1: factory called per request
    runtime.block_on(dispatch(router, &envelope)) // pre-P2: dispatch clones envelope internally
}

/// Simulates the post-patch hot path explicitly so the comparison
/// against `naive_dispatch_from_json` is apples-to-apples (no detour
/// through the global `APP_ROUTER` `OnceLock`).
fn patched_dispatch_from_json(input: &str, runtime: &Runtime, cached_router: &Router) -> String {
    let envelope = parse_request(input).expect("valid envelope");
    let router = cached_router.clone(); // post-P1: cheap Arc-backed clone
    let response = runtime.block_on(dispatch_owned(router, envelope));
    serde_json::to_string(&response).expect("response is serializable")
}

// ── Benchmarks ───────────────────────────────────────────────────────

/// P1 isolation: cached Router::clone() vs factory rebuild per call.
/// Dispatch step is identical (`dispatch_owned`) on both sides so any
/// delta is attributable to router construction.
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

/// P2 isolation: `dispatch_owned` (envelope moved into HTTP request) vs
/// `dispatch_typed` (envelope borrowed → clone then `dispatch_owned`
/// internally).  Each iteration **freshly parses** the envelope from JSON
/// so the owned path genuinely avoids a clone; the borrowed path pays
/// for exactly one extra `RequestEnvelope::clone()` inside
/// `dispatch_typed`.  Both arms return `ResponseEnvelope` so the
/// response-JSON serialization cost is excluded.
fn bench_dispatch_path(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let cached = build_router(20);
    let mut group = c.benchmark_group("dispatch_path");

    for &body_kb in &[1_usize, 64, 1024] {
        let envelope_json = make_envelope_json(body_kb);
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        group.bench_with_input(
            BenchmarkId::new("owned_post_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    let env = parse_request(&envelope_json).expect("valid envelope");
                    runtime.block_on(dispatch_owned(cached.clone(), env))
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("borrowed_pre_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    let env = parse_request(&envelope_json).expect("valid envelope");
                    runtime.block_on(dispatch_typed(cached.clone(), &env))
                });
            },
        );
    }

    group.finish();
}

/// End-to-end JNI-style flow: JSON in → JSON out.  Combines P1 + P2 so
/// the headline “Router rebuild + body clone” cost is visible.
fn bench_full_flow(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let cached_100 = build_router(100);
    let mut group = c.benchmark_group("full_flow");

    for &body_kb in &[1_usize, 64, 1024] {
        let envelope_json = make_envelope_json(body_kb);
        group.throughput(Throughput::Bytes((body_kb * 1024) as u64));

        group.bench_with_input(
            BenchmarkId::new("patched_post_P1_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| patched_dispatch_from_json(&envelope_json, &runtime, &cached_100));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("naive_pre_P1_P2", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    naive_dispatch_from_json(&envelope_json, &runtime, &|| build_router(100))
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_router_path, bench_dispatch_path, bench_full_flow);
criterion_main!(benches);
