//! Deterministic **allocation-budget gate** for the in-process dispatch
//! hot paths.
//!
//! The criterion timing benches drift ±8–10 % on shared CI runners, so
//! the `bench.yml` gate can only fire at a loose ±10 % threshold — a
//! genuine sub-10 % regression slips through.  The *number of heap
//! allocations per dispatch*, by contrast, is **deterministic**: identical
//! inputs allocate identically on every run, every machine.  A global
//! counting allocator records `alloc` / `realloc` calls so these tests
//! assert an exact per-op allocation budget — catching an accidental new
//! allocation (or a `Vec` that starts reallocating because a capacity
//! estimate regressed) at **zero noise**.  This is the Rust-side analogue
//! of the Java `PerfAllocBench`'s `getThreadAllocatedBytes` approach.
//!
//! Budgets are **upper bounds**: a change that REMOVES allocations passes
//! (and should then tighten the budget); a change that ADDS one fails.
//!
//! All measurements run inside ONE `#[test]` so they execute
//! single-threaded — libtest runs test fns concurrently by default and the
//! allocator counter is process-global, so separate test fns would race.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::routing::{get, post};
use bytes::Bytes;
use serde_json::json;
use tokio::runtime::{Builder, Runtime};
use vespera_inprocess::{
    dispatch_from_bytes, dispatch_into, dispatch_into_async_borrowed, register_app,
};

// ── Counting global allocator ────────────────────────────────────────

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method delegates to the `System` allocator with the exact
// same arguments; we only bump relaxed counters first, which cannot affect
// allocation correctness.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ── Fixtures ─────────────────────────────────────────────────────────

async fn ping() -> &'static str {
    "pong"
}

async fn echo(body: Bytes) -> Bytes {
    body
}

/// Returns a `422` with a JSON `{"errors":[...]}` body so the wire path
/// exercises the `to_wire_bytes` 422 `validation_errors` hoist plus the
/// header-capacity estimate. Two realistic errors push the hoisted header
/// JSON past the `WIRE_HEADER_RESERVE` floor, so a build whose capacity
/// estimate ignores the hoisted errors reallocates once mid-serialize; the
/// validation-errors capacity estimate removes that realloc.
async fn validate_fail() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        r#"{"errors":[{"path":"username","message":"length is lower than 3"},{"path":"email","message":"not a valid email"}]}"#,
    )
        .into_response()
}

fn install() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_app(|| {
            Router::new()
                .route("/ping", get(ping))
                .route("/echo", post(echo))
                .route("/validate", post(validate_fail))
        });
    });
}

fn runtime() -> Runtime {
    Builder::new_current_thread().enable_all().build().unwrap()
}

/// Assemble `[u32 BE header_len | header JSON | body]` wire bytes with an
/// arbitrary request-header set.
fn encode(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let header_map: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
        .collect();
    let header = json!({ "v": 1, "method": method, "path": path, "headers": header_map });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

/// One measured allocation sample: per-op `alloc` calls, `realloc` calls,
/// and bytes requested, averaged over `iters` after `warmup` settling ops.
struct Sample {
    allocs: usize,
    reallocs: usize,
    bytes: usize,
}

/// Run `op` `warmup` times to settle one-time lazy initialisation
/// (`OnceLock` routers / config), then measure `iters` ops against the
/// global counters.  Allocation counts are deterministic, so integer
/// division yields the exact per-op figure.
fn measure(warmup: usize, iters: usize, mut op: impl FnMut()) -> Sample {
    for _ in 0..warmup {
        op();
    }
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let r0 = REALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    for _ in 0..iters {
        op();
    }
    Sample {
        allocs: (ALLOCS.load(Ordering::Relaxed) - a0) / iters,
        reallocs: (REALLOCS.load(Ordering::Relaxed) - r0) / iters,
        bytes: (BYTES.load(Ordering::Relaxed) - b0) / iters,
    }
}

const HEADERS_16: &[(&str, &str)] = &[
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

#[test]
fn allocation_budgets() {
    install();
    let rt = runtime();
    let mut out = vec![0u8; 64 * 1024];

    // ── Case A: bodyless GET via the borrowed direct-write path. No input
    // clone (borrows `wire`), no output `Vec` (writes into `out`), no body
    // copy — isolates the pure per-dispatch allocation floor.
    let wire_get = encode("GET", "/ping", &[], &[]);
    let bodyless = measure(200, 2000, || {
        let _ = rt.block_on(dispatch_into_async_borrowed(&wire_get, &mut out));
    });

    // ── Case B: small POST echo (borrowed). Adds the one body-copy
    // allocation (`Bytes::copy_from_slice`) over Case A.
    let wire_post = encode(
        "POST",
        "/echo",
        &[("content-type", "application/json")],
        br#"{"k":1}"#,
    );
    let small_post = measure(200, 2000, || {
        let _ = rt.block_on(dispatch_into_async_borrowed(&wire_post, &mut out));
    });

    // ── Case C: 16-header POST (borrowed). Locks the request-header
    // handling allocation count — guards the content-type-scan fusion and
    // any future header-path allocation regression.  The request-header pair
    // `Vec` is now pre-reserved at `TYPICAL_HEADER_CAP` (16), so a 16-header
    // request fills WITHOUT the realloc the previous capacity-8 reserve paid
    // (40 alloc + 0 realloc; was 40 alloc + 1 realloc).
    let wire_hdrs = encode("POST", "/echo", HEADERS_16, br#"{"k":1}"#);
    let headers_post = measure(200, 2000, || {
        let _ = rt.block_on(dispatch_into_async_borrowed(&wire_hdrs, &mut out));
    });

    // ── Case D: bodyless GET via the buffered materialise path
    // (`dispatch_from_bytes`). Includes the input `wire.clone()` and the
    // response `Vec` allocation the direct-write path avoids — guards the
    // primary FFI entry point.
    let materialise = measure(200, 2000, || {
        let _ = dispatch_from_bytes(wire_get.clone(), &rt);
    });

    // ── Case E: bodyless GET via `dispatch_into` (owned input clone, reused
    // out buffer) — the JNI `dispatchDirect` sync path shape.
    let direct_into = measure(200, 2000, || {
        let _ = dispatch_into(wire_get.clone(), &mut out, &rt);
    });

    // ── Case F: 422 JSON response via the buffered materialise path. The
    // `to_wire_bytes` 422 path hoists `{"errors":[...]}` into the wire
    // header's `validation_errors`. With the hoisted-errors length folded
    // into the capacity estimate the response `Vec` is sized to serialise the
    // header in one shot — the realloc a hoist-blind estimate paid is gone.
    let wire_validate = encode(
        "POST",
        "/validate",
        &[("content-type", "application/json")],
        br#"{"x":1}"#,
    );
    let validate_422 = measure(200, 2000, || {
        let _ = dispatch_from_bytes(wire_validate.clone(), &rt);
    });

    // (label, sample, budget). The gate metric is total per-op allocation
    // OPS (`alloc` + `realloc` calls) — the deterministic, noise-free
    // figure; bytes/op is informational only.
    let cases = [
        (
            "A bodyless-GET borrowed",
            &bodyless,
            BUDGET_BODYLESS_BORROWED,
        ),
        ("B small-POST borrowed", &small_post, BUDGET_SMALL_POST),
        (
            "C 16-header-POST borrowed",
            &headers_post,
            BUDGET_HEADERS_POST,
        ),
        (
            "D bodyless-GET materialise",
            &materialise,
            BUDGET_MATERIALISE,
        ),
        (
            "E bodyless-GET dispatch_into",
            &direct_into,
            BUDGET_DISPATCH_INTO,
        ),
        (
            "F 422-validate materialise",
            &validate_422,
            BUDGET_VALIDATE_422,
        ),
    ];

    // Print every case first so a regression failure still shows the full
    // picture (the asserts below would otherwise stop at the first miss).
    for &(name, sample, budget) in &cases {
        eprintln!(
            "VESPERA_ALLOC {name}: allocs/op={} reallocs/op={} bytes/op={} ops={} (budget {budget})",
            sample.allocs,
            sample.reallocs,
            sample.bytes,
            sample.allocs + sample.reallocs
        );
    }
    for &(name, sample, budget) in &cases {
        let ops = sample.allocs + sample.reallocs;
        assert!(
            ops <= budget,
            "{name} allocation regressed: {ops} alloc-ops/op \
             (allocs={}, reallocs={}) exceeds budget {budget}",
            sample.allocs,
            sample.reallocs
        );
    }
}

// Budgets — total per-op allocation OPS (`alloc` + `realloc` calls),
// measured 2026-06 and verified identical across repeated runs (allocation
// counts are deterministic). UPPER BOUNDS: a change that ADDS an allocation
// trips the matching assert; one that REMOVES allocations passes and SHOULD
// then tighten the constant. Most of each count is axum `router.oneshot` +
// tokio `block_on` (framework), not vespera wire code — the gate guards
// against ADDING to the per-dispatch floor.
//
// BUDGET_HEADERS_POST is now realloc-free: the request-header `Vec` is
// pre-reserved at `TYPICAL_HEADER_CAP` (16), so the 16-header set fills
// without the capacity-8 growth realloc it previously paid.  An under-reserve
// regression (a re-introduced realloc, or extra allocs) trips this budget.
const BUDGET_BODYLESS_BORROWED: usize = 14; // borrowed: no clone / no output Vec / no body copy
const BUDGET_SMALL_POST: usize = 22; // borrowed: +1 body copy over bodyless
const BUDGET_HEADERS_POST: usize = 40; // borrowed: 40 alloc + 0 realloc (header Vec pre-reserved at 16)
// MATERIALISE / DISPATCH_INTO dropped by 2 each (was 18 / 17) when the OWNED
// wire path stopped copying the request path into a fresh `Bytes`: a bodyless
// GET's borrowed path now SHARES the request's owning header `Bytes` to build
// the `Uri` (`Uri::from_maybe_shared` via `slice_from_owner`), removing the
// `Uri::try_from(&str)` allocation+copy.  A regression that re-introduces the
// path copy (or any other owned-path allocation) trips these tightened budgets.
const BUDGET_MATERIALISE: usize = 16; // dispatch_from_bytes: +input clone +response Vec, URI shared
const BUDGET_DISPATCH_INTO: usize = 15; // dispatch_into: +input clone, reused out, URI shared
// 422 materialise path: the hoisted `validation_errors` JSON is now folded
// into the response-`Vec` capacity estimate, so the wire header serialises
// without the mid-write realloc a hoist-blind estimate paid (26 alloc + 0
// realloc; was 26 alloc + 1 realloc). A re-introduced realloc trips this.
const BUDGET_VALIDATE_422: usize = 26; // realloc-free 422 hoist (was 27 w/ realloc)
