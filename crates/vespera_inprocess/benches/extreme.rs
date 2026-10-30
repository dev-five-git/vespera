//! Large-scale / extreme-input criterion benchmarks for the in-process dispatch
//! surface (opt-in: NOT part of the PR bench gate, which runs `--bench dispatch`).
//!
//! Micro benches hide per-op costs that compound (or go non-linear) at scale,
//! so every group here is a size ladder; read each ladder as ns/op and ns per
//! unit (route / header / error / byte) and check that the per-unit figure is
//! flat. Allocation counts and peak heap come from a counting allocator that
//! wraps mimalloc (the shipped cdylib allocator) and are printed by the
//! `report` helper for the memory-bounded streaming paths.
//!
//! ```text
//! cargo bench -p vespera_inprocess --bench extreme                  # default ladder
//! VESPERA_EXTREME=1 cargo bench -p vespera_inprocess --bench extreme  # + 100 MiB / 100k in-flight / 10k apps / 100k errors
//! cargo bench -p vespera_inprocess --bench extreme -- headers       # filter
//! ```
//!
//! Groups: `route_table`, `request_headers`, `request_header_value`,
//! `response_headers`, `errors_422`, `path_query`, `body_echo`,
//! `stream_response`, `stream_upload`, `concurrent`, `registry`.

use std::alloc::{GlobalAlloc, Layout};
use std::fmt::Write as _;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use http_body_util::BodyExt;
use tokio::runtime::Runtime;
use vespera_inprocess::{
    RequestChunk, dispatch_bidirectional_streaming, dispatch_from_bytes, dispatch_from_bytes_async,
    dispatch_streaming_async, register_app_named,
};

struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static CURRENT: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call unchanged to mimalloc; the atomics only observe sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            let size = signed(layout.size());
            let now = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        unsafe { mimalloc::MiMalloc.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::Relaxed) {
            CURRENT.fetch_sub(signed(layout.size()), Ordering::Relaxed);
        }
        unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            let delta = signed(new_size) - signed(layout.size());
            let now = CURRENT.fetch_add(delta, Ordering::Relaxed) + delta;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        unsafe { mimalloc::MiMalloc.realloc(ptr, layout, new_size) }
    }
}

fn signed(size: usize) -> isize {
    isize::try_from(size).unwrap_or(isize::MAX)
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Run `f` once with allocation counting on; print allocs and peak heap above
/// the baseline live bytes. Returns `(allocs, peak_bytes)`.
fn report<R>(label: &str, f: impl FnOnce() -> R) -> (usize, usize) {
    let base = CURRENT.load(Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let started = Instant::now();
    let _ = std::hint::black_box(f());
    let elapsed = started.elapsed();
    COUNTING.store(false, Ordering::Relaxed);
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let peak = usize::try_from(PEAK.load(Ordering::Relaxed).saturating_sub(base)).unwrap_or(0);
    eprintln!(
        "[extreme] {label}: {allocs} allocs, peak heap +{} KiB, {elapsed:?}",
        peak / 1024
    );
    (allocs, peak)
}

fn extreme() -> bool {
    std::env::var("VESPERA_EXTREME").is_ok_and(|v| v == "1")
}

fn ladder(base: &[usize], extra: &[usize]) -> Vec<usize> {
    let mut sizes = base.to_vec();
    if extreme() {
        sizes.extend_from_slice(extra);
    }
    sizes
}

fn heavy(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));
}

// -- Fixture app --

async fn ok() -> &'static str {
    "ok"
}

async fn echo_bytes(body: Bytes) -> Bytes {
    body
}

/// Handler fixtures are built once per size so the benches measure dispatch,
/// not the handler's own `format!` / header-parse work.
fn cached<T: Clone + Send + 'static>(
    cache: &'static std::sync::OnceLock<Mutex<std::collections::HashMap<usize, T>>>,
    n: usize,
    build: impl FnOnce() -> T,
) -> T {
    let mut map = cache
        .get_or_init(Mutex::default)
        .lock()
        .expect("fixture cache");
    map.entry(n).or_insert_with(build).clone()
}

async fn many_headers(Path(n): Path<usize>) -> Response {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, HeaderMap>>> =
        std::sync::OnceLock::new();
    let headers = cached(&CACHE, n, || {
        let mut map = HeaderMap::new();
        for i in 0..n {
            map.insert(
                HeaderName::try_from(format!("x-h-{i}")).expect("header name"),
                HeaderValue::try_from(format!("value-{i}-abcdefghijklmnop")).expect("header value"),
            );
        }
        map
    });
    (headers, "ok").into_response()
}

async fn errors_422(Path(n): Path<usize>) -> Response {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, Bytes>>> =
        std::sync::OnceLock::new();
    let body = cached(&CACHE, n, || {
        let mut body = String::with_capacity(16 + n * 64);
        body.push_str(r#"{"errors":["#);
        for i in 0..n {
            if i > 0 {
                body.push(',');
            }
            let _ = write!(
                body,
                r#"{{"path":"items[{i}].field","message":"value {i} is not valid"}}"#
            );
        }
        body.push_str("]}");
        Bytes::from(body)
    });
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [("content-type", "application/json")],
        body,
    )
        .into_response()
}
async fn stream_body(Path(mib): Path<usize>) -> Response {
    const CHUNK: usize = 64 * 1024;
    let count = mib * 1024 * 1024 / CHUNK;
    let stream = futures_util::stream::iter(
        (0..count).map(|_| Ok::<Bytes, std::convert::Infallible>(Bytes::from(vec![0xA5u8; CHUNK]))),
    );
    Response::new(Body::from_stream(stream))
}

async fn sink(body: Body) -> String {
    let mut total = 0usize;
    let mut stream = body;
    while let Some(Ok(frame)) = stream.frame().await {
        if let Ok(data) = frame.into_data() {
            total += data.len();
        }
    }
    total.to_string()
}

fn build_app(routes: usize) -> Router {
    let mut table = Router::new()
        .route("/ok", get(ok))
        .route("/echo/bytes", post(echo_bytes))
        .route("/headers/{n}", get(many_headers))
        .route("/errors/{n}", get(errors_422))
        .route("/stream/{mib}", get(stream_body))
        .route("/sink", post(sink))
        .layer(axum::extract::DefaultBodyLimit::disable());
    for i in 0..routes {
        table = table.route(&format!("/r/{i}"), get(ok));
    }
    table
}

fn install_default() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = vespera_inprocess::set_max_request_bytes(0);
        vespera_inprocess::register_app_named("_default", || build_app(0));
    });
}

// -- Wire helpers --

fn wire_json(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    app: Option<&str>,
    body: &[u8],
) -> Vec<u8> {
    let header_map: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let mut header = serde_json::json!({ "v": 1, "method": method, "path": path });
    if !query.is_empty() {
        header["query"] = serde_json::Value::String(query.to_owned());
    }
    if !headers.is_empty() {
        header["headers"] = serde_json::Value::Object(header_map);
    }
    if let Some(app) = app {
        header["app"] = serde_json::Value::String(app.to_owned());
    }
    let header_bytes = serde_json::to_vec(&header).expect("header json");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(
        &u32::try_from(header_bytes.len())
            .expect("len")
            .to_be_bytes(),
    );
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

/// HTTP status carried in a binary-wire response header.
fn status_of(response: &[u8]) -> u64 {
    let len = u32::from_be_bytes(response[..4].try_into().expect("len prefix")) as usize;
    let header: serde_json::Value = serde_json::from_slice(&response[4..4 + len]).expect("header");
    header["status"].as_u64().expect("status")
}

/// Fail loudly if a bench case is not measuring the intended (2xx/422) path.
fn expect_status(response: &[u8], expected: u64, what: &str) {
    let actual = status_of(response);
    assert_eq!(
        actual,
        expected,
        "{what}: unexpected status; response = {}",
        String::from_utf8_lossy(&response[..response.len().min(400)])
    );
}

fn get_wire(path: &str) -> Vec<u8> {
    wire_json("GET", path, "", &[], None, &[])
}

// -- Groups --

/// Huge route tables: Router build, and lookup of the first / last route.
/// Per-route lookup cost must be flat (matchit radix tree), build ~linear.
fn bench_route_table(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("route_table");
    heavy(&mut group);
    for n in ladder(&[1_000, 10_000], &[50_000]) {
        let name = format!("rt-{n}");
        register_app_named(&name, move || build_app(n));
        group.bench_with_input(BenchmarkId::new("build", n), &n, |b, &n| {
            b.iter(|| build_app(n));
        });
        for (label, idx) in [("first", 0), ("last", n - 1)] {
            let wire = wire_json("GET", &format!("/r/{idx}"), "", &[], Some(&name), &[]);
            expect_status(
                &dispatch_from_bytes(wire.clone(), &runtime),
                200,
                "route_table",
            );
            group.bench_with_input(
                BenchmarkId::new(format!("dispatch_{label}"), n),
                &n,
                |b, _| {
                    b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
                },
            );
        }
    }
    group.finish();
}

/// Many request headers (parse + HeaderMap build): ns/header must be flat.
fn bench_request_headers(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("request_headers");
    heavy(&mut group);
    for n in ladder(&[10, 100, 1_000, 5_000], &[10_000]) {
        let headers: Vec<(String, String)> = (0..n)
            .map(|i| (format!("x-req-{i}"), format!("value-{i}-abcdefghijklmnop")))
            .collect();
        let wire = wire_json("GET", "/ok", "", &headers, None, &[]);
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            200,
            "request_headers",
        );
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }
    group.finish();
}

/// One header with a very large value (the wire header is capped at 1 MiB): plain ASCII, Unicode-heavy (2/3/4-byte
/// UTF-8, escaped surrogate pairs) and escape-heavy (quote, backslash, control).
fn bench_request_header_value(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("request_header_value");
    heavy(&mut group);
    for size in ladder(&[1_024, 65_536, 400_000], &[]) {
        for (kind, unit) in [
            ("ascii", "abcdefghij"),
            ("unicode", "\u{e9}\u{20ac}\u{1d11e}\u{d55c}\u{ae00}"),
            ("escapes", "a\"b\\c\t"),
        ] {
            let value = unit.repeat(size / unit.len().max(1));
            let wire = wire_json("GET", "/ok", "", &[("x-big".to_owned(), value)], None, &[]);
            expect_status(&dispatch_from_bytes(wire.clone(), &runtime), 200, kind);
            group.throughput(Throughput::Bytes(wire.len() as u64));
            group.bench_with_input(BenchmarkId::new(kind, size), &size, |b, _| {
                b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
            });
        }
    }
    group.finish();
}

/// Many response headers (collect + wire header serialise).
fn bench_response_headers(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("response_headers");
    heavy(&mut group);
    for n in ladder(&[10, 100, 1_000, 5_000], &[10_000]) {
        let wire = get_wire(&format!("/headers/{n}"));
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            200,
            "response_headers",
        );
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }
    group.finish();
}

/// `422` validation envelopes with many entries. Bodies up to 64 KiB (~900 of
/// these entries) are hoisted into the wire header as `validation_errors`; larger
/// ones pass through verbatim (documented cap) - the ladder crosses that cliff.
fn bench_errors_422(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("errors_422");
    heavy(&mut group);
    for n in ladder(&[10, 100, 900], &[10_000, 100_000]) {
        let wire = get_wire(&format!("/errors/{n}"));
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            422,
            "errors_422",
        );
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }
    group.finish();
}

/// Long paths (404 route miss) and long query strings (`http::Uri` caps at 64 KiB).
fn bench_path_query(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("path_query");
    heavy(&mut group);
    for size in ladder(&[1_024, 8_192, 32_768], &[]) {
        let path = format!("/{}", "p".repeat(size));
        let wire = get_wire(&path);
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            404,
            "long_path",
        );
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("long_path_404", size), &size, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
        let query = (0..size / 12)
            .map(|i| format!("k{i}=v{i}"))
            .collect::<Vec<_>>()
            .join("&");
        let wire = wire_json("GET", "/ok", &query, &[], None, &[]);
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            200,
            "long_query",
        );
        group.bench_with_input(BenchmarkId::new("long_query", size), &size, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }
    group.finish();
}

/// Big buffered bodies through `dispatch_from_bytes` (echo).
fn bench_body_echo(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("body_echo");
    heavy(&mut group);
    for mib in ladder(&[1, 16], &[100]) {
        let payload = vec![0xA5u8; mib * 1024 * 1024];
        let wire = wire_json(
            "POST",
            "/echo/bytes",
            "",
            &[(
                "content-type".to_owned(),
                "application/octet-stream".to_owned(),
            )],
            None,
            &payload,
        );
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            200,
            "body_echo",
        );
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(mib), &mib, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
        report(&format!("body_echo {mib} MiB (buffered)"), || {
            dispatch_from_bytes(wire.clone(), &runtime)
        });
    }
    group.finish();
}

/// Response streaming of a generated body: throughput plus a peak-heap check -
/// the heap high-water mark must stay chunk-bounded regardless of body size.
fn bench_stream_response(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("stream_response");
    heavy(&mut group);
    for mib in ladder(&[16], &[100, 1_000]) {
        let wire = get_wire(&format!("/stream/{mib}"));
        group.throughput(Throughput::Bytes((mib * 1024 * 1024) as u64));
        let run = || {
            let mut seen = 0usize;
            runtime.block_on(dispatch_streaming_async(wire.clone(), |chunk| {
                seen += chunk.len();
                ControlFlow::Continue(())
            }));
            assert_eq!(seen, mib * 1024 * 1024, "stream_response: short body");
            seen
        };
        group.bench_with_input(BenchmarkId::from_parameter(mib), &mib, |b, _| b.iter(run));
        report(&format!("stream_response {mib} MiB"), run);
    }
    group.finish();
}

/// Bidirectional streaming upload into a sink handler: request bytes flow through
/// the bounded channel; peak heap must stay `O(chunk * channel capacity)`.
fn bench_stream_upload(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("stream_upload");
    heavy(&mut group);
    let chunk_len = vespera_inprocess::DEFAULT_STREAMING_CHUNK_BYTES;
    for mib in ladder(&[16], &[100, 1_000]) {
        let header_only = wire_json("POST", "/sink", "", &[], None, &[]);
        group.throughput(Throughput::Bytes((mib * 1024 * 1024) as u64));
        let run = || {
            let remaining = Mutex::new(mib * 1024 * 1024);
            let pull = move || -> RequestChunk {
                let mut remaining = remaining.lock().expect("lock");
                if *remaining == 0 {
                    return RequestChunk::End;
                }
                let len = (*remaining).min(chunk_len);
                *remaining -= len;
                RequestChunk::Data(vec![0xA5u8; len])
            };
            let mut echoed = Vec::new();
            runtime.block_on(dispatch_bidirectional_streaming(
                header_only.clone(),
                pull,
                |chunk| {
                    echoed.extend_from_slice(chunk);
                    ControlFlow::Continue(())
                },
            ));
            assert_eq!(
                String::from_utf8_lossy(&echoed),
                (mib * 1024 * 1024).to_string(),
                "stream_upload: sink saw a different byte count"
            );
            echoed.len()
        };
        group.bench_with_input(BenchmarkId::from_parameter(mib), &mib, |b, _| b.iter(run));
        report(&format!("stream_upload {mib} MiB"), run);
    }
    group.finish();
}

/// Many concurrent in-flight dispatches on a multi-thread runtime.
fn bench_concurrent(c: &mut Criterion) {
    install_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut group = c.benchmark_group("concurrent");
    heavy(&mut group);
    let wire = get_wire("/ok");
    for n in ladder(&[1_000, 10_000], &[100_000]) {
        group.throughput(Throughput::Elements(n as u64));
        let run = || {
            runtime.block_on(async {
                let mut set = tokio::task::JoinSet::new();
                for _ in 0..n {
                    set.spawn(dispatch_from_bytes_async(wire.clone()));
                }
                let mut done = 0usize;
                while let Some(result) = set.join_next().await {
                    let response = result.expect("task");
                    done += usize::from(status_of(&response) == 200);
                }
                assert_eq!(done, n, "concurrent: non-200 responses");
                done
            })
        };
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| b.iter(run));
        report(&format!("concurrent {n} in flight"), run);
    }
    group.finish();
}

/// Registry scaling: total registration cost for K named apps (copy-on-write
/// map: watch for O(K^2)) and dispatch-by-name cost with K apps registered.
fn bench_registry(c: &mut Criterion) {
    install_default();
    let runtime = Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("registry");
    heavy(&mut group);
    let mut registered = 0usize;
    for k in ladder(&[100, 500, 2_000], &[10_000]) {
        let started = Instant::now();
        for i in registered..k {
            register_app_named(&format!("app-{i}"), Router::new);
        }
        eprintln!(
            "[extreme] registry: {} -> {k} apps registered in {:?}",
            registered,
            started.elapsed()
        );
        registered = k;
        let wire = wire_json(
            "GET",
            "/nothing",
            "",
            &[],
            Some(&format!("app-{}", k / 2)),
            &[],
        );
        expect_status(
            &dispatch_from_bytes(wire.clone(), &runtime),
            404,
            "registry",
        );
        group.bench_with_input(BenchmarkId::new("dispatch_by_name", k), &k, |b, _| {
            b.iter(|| dispatch_from_bytes(wire.clone(), &runtime));
        });
    }
    group.finish();
}

/// Thread scaling of the synchronous entry point: `T` OS threads, each with its own
/// current-thread runtime (the JNI `dispatchBytes` shape), hammer `dispatch_from_bytes`
/// for a fixed window. Prints total ops/s and efficiency vs. one thread (ideal = 100%).
fn bench_thread_scaling(_c: &mut Criterion) {
    const WINDOW_MS: u64 = 1_500;
    install_default();
    let wire = get_wire("/ok");
    let window = Duration::from_millis(WINDOW_MS);
    let mut base = 0u64;
    for threads in ladder(&[1, 2, 4, 8, 16], &[32, 64]) {
        let total: u64 = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .max_blocking_threads(4)
                            .build()
                            .expect("runtime");
                        let started = Instant::now();
                        let mut ops = 0u64;
                        while started.elapsed() < window {
                            for _ in 0..256 {
                                std::hint::black_box(dispatch_from_bytes(wire.clone(), &runtime));
                            }
                            ops += 256;
                        }
                        ops
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).sum()
        });
        let per_sec = total * 1_000 / WINDOW_MS;
        if threads == 1 {
            base = per_sec;
        }
        let ideal = base * u64::try_from(threads).unwrap_or(u64::MAX);
        eprintln!(
            "[extreme] thread_scaling threads={threads}: {per_sec} ops/s total, efficiency {}%",
            100 * per_sec / ideal.max(1)
        );
    }
}

criterion_group!(
    benches,
    bench_thread_scaling,
    bench_route_table,
    bench_request_headers,
    bench_request_header_value,
    bench_response_headers,
    bench_errors_422,
    bench_path_query,
    bench_body_echo,
    bench_stream_response,
    bench_stream_upload,
    bench_concurrent,
    bench_registry,
);
criterion_main!(benches);
