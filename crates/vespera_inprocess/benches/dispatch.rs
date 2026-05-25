//! Criterion benchmarks for the in-process dispatch surface.
//!
//! Three groups:
//!
//! - `router_path`: `Router::clone()` of a pre-built router  (post-P1)
//!   vs rebuilding the router from a factory closure        (pre-P1, simulated).
//! - `dispatch_path`: `dispatch_owned(router, env)`          (post-P2)
//!   vs `dispatch_typed(router, &env)` which clones internally (pre-P2).
//! - `wire_path`: end-to-end `dispatch_from_bytes` — wire-format
//!   round-trip including header JSON parse + body byte handling.
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
use vespera_inprocess::{
    RequestEnvelope, dispatch_from_bytes, dispatch_owned, dispatch_typed, register_app,
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

/// Build a router with `n_routes` distinct GET endpoints plus one
/// `POST /echo` that echoes the request body.
fn build_router(n_routes: usize) -> Router {
    let mut router = Router::new().route("/echo", post(handler_echo));
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

/// Wire-format request payload for the `dispatch_from_bytes` bench.
fn make_wire_request(body_kb: usize) -> Vec<u8> {
    let body_str = serde_json::to_string(&Echo {
        body: "x".repeat(body_kb * 1024),
    })
    .unwrap();
    let header = serde_json::json!({
        "v": 1,
        "method": "POST",
        "path": "/echo",
        "headers": {"content-type": "application/json"},
    });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let header_len = u32::try_from(header_bytes.len()).unwrap();
    let body_bytes = body_str.as_bytes();
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body_bytes);
    wire
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
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| register_app(|| build_router(100)));

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

criterion_group!(
    benches,
    bench_router_path,
    bench_dispatch_path,
    bench_wire_path
);
criterion_main!(benches);
