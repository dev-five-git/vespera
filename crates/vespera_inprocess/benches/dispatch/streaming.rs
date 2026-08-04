//! Streaming-family criterion benchmark groups.
//!
//! `streaming_path`, `async_spawn_pattern`, `close_hook_ab` and
//! `async_completion_ab` — split out of `dispatch.rs` verbatim to keep that
//! file under the 1000-line cap (same reason as the sibling `serde_ab`
//! module). Unlike `serde_ab` these groups are NOT feature-gated: they run in
//! the default bench build and stay wired into the parent `benches` criterion
//! group, so the measured work and every group/bench id is unchanged.

// This is a `#[path]` bench submodule of `dispatch.rs`; it intentionally
// re-uses the parent bench file's imports (criterion types, tokio `Runtime`,
// the wire-assembly + app-install helpers) rather than re-listing them.  The
// glob is the idiomatic shape for a bench helper split out only to honour the
// file-size cap.
#[allow(clippy::wildcard_imports)]
use super::*;

/// P1/P3 isolation: streaming dispatch throughput.
///
/// - `response_streaming`: full body in the request, response drained
///   through the `on_chunk` callback.
/// - `bidirectional`: request body fed through `pull_chunk` in
///   [`vespera_inprocess::DEFAULT_STREAMING_CHUNK_BYTES`] pieces
///   (mirrors the JNI `InputStream` reader), response drained through
///   `on_chunk` — exercises the bounded mpsc channel and the
///   `spawn_blocking` producer.
pub fn bench_streaming_path(c: &mut Criterion) {
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
pub fn bench_async_spawn_pattern(c: &mut Criterion) {
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

/// Same-run A/B for the `RequestSourceCloser` hardening: the request-source
/// close hook is now invoked under `catch_unwind` so a panicking hook running
/// from `Drop` during unwind cannot double-panic -> `abort()` the host JVM.
/// This isolates the added `catch_unwind` landing-pad cost vs a direct call,
/// with BOTH arms in the SAME run so the measurement is immune to the
/// cross-run thermal/load drift that swamps the dispatch-level `streaming_path`
/// comparison (the close hook fires once per bidirectional dispatch, after the
/// response body is fully drained, so its cost is amortised over an entire
/// dispatch — this micro-A/B is the only instrument fine enough to resolve it).
pub fn bench_close_hook_ab(c: &mut Criterion) {
    use std::panic::AssertUnwindSafe;
    let mut group = c.benchmark_group("close_hook_ab");

    // `pre`: the previous direct `close()` call.  `post`: the hardened
    // `catch_unwind(AssertUnwindSafe(close))`.  The closure does a tiny
    // black-boxed op so it is neither optimised away nor large enough to
    // dwarf the landing-pad cost being measured.
    group.bench_function("direct_call_pre", |b| {
        b.iter(|| {
            let f = || std::hint::black_box(1u64).wrapping_mul(3);
            std::hint::black_box(f())
        });
    });

    group.bench_function("catch_unwind_post", |b| {
        b.iter(|| {
            let f = || std::hint::black_box(1u64).wrapping_mul(3);
            std::hint::black_box(std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or(0))
        });
    });

    group.finish();
}

/// Same-run A/B for the Oracle-flagged `dispatchAsync` completion-isolation
/// question: does completing the Java `CompletableFuture` from a
/// `spawn_blocking` thread (so a blocking / re-entrant Java continuation runs
/// OFF the core Tokio workers) cost enough to matter on the async path?
///
/// - `complete_inline_pre`: the future is completed inline on the dispatch
///   worker (the pre-change behaviour) — no isolation hop.
/// - `complete_spawn_blocking_post`: the completion is moved to a
///   `spawn_blocking` thread — isolates Java continuations from the core
///   workers at the cost of one blocking-pool hand-off.
///
/// Both arms run in the SAME run (drift-immune).  The delta is the per-async-
/// dispatch cost isolation would add, and decides whether to isolate
/// unconditionally or document the `thenApplyAsync` contract instead (speed is
/// the stated priority, so a large hop argues for the zero-cost doc contract).
///
/// VERDICT (measured, AMD Ryzen 9 9950X): `complete_inline_pre` ~1.5 µs vs
/// `complete_spawn_blocking_post` ~24.5 µs — a ~16x per-dispatch regression.
/// Forced isolation is therefore REJECTED (it violates the speed-first
/// priority); the worker-thread completion is kept and the threading contract
/// is documented on `dispatchAsync` instead (callers use `*Async` continuations
/// and avoid blocking / re-entrant inline continuations).  This A/B stays as
/// the permanent regression-decision guard so the 16x cost is not re-discovered.
pub fn bench_async_completion_isolation_ab(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let mut group = c.benchmark_group("async_completion_ab");

    group.bench_function("complete_inline_pre", |b| {
        b.iter(|| {
            runtime.block_on(async {
                tokio::spawn(async move {
                    let resp = std::hint::black_box(vec![0u8; 64]);
                    std::hint::black_box(resp.len())
                })
                .await
                .unwrap()
            })
        });
    });

    group.bench_function("complete_spawn_blocking_post", |b| {
        b.iter(|| {
            runtime.block_on(async {
                tokio::spawn(async move {
                    let resp = std::hint::black_box(vec![0u8; 64]);
                    tokio::task::spawn_blocking(move || std::hint::black_box(resp.len()))
                        .await
                        .unwrap()
                })
                .await
                .unwrap()
            })
        });
    });

    group.finish();
    drop(runtime);
}
