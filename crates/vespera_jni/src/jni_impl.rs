use std::{
    future::Future,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{Global, JByteArray, JClass, JObject};
use jni::sys::{jbyteArray, jint};

use crate::daemon_env::with_cached_daemon_env;
use crate::streaming_closures::{
    call_header_consumer, close_input_stream, complete_future, complete_future_local,
    make_pull_closure, make_push_closure,
};

// Per-thread reusable Java chunk buffers for the streaming paths live in
// a sidecar module to keep this file within the 1000-line source cap.
#[path = "jni_impl_streaming_buffer.rs"]
mod streaming_buffer;
use streaming_buffer::{
    PullPushBuffers, StreamingBufferRole, checkout_pull_push_buffers,
    checkout_streaming_chunk_buffer, mark_streaming_buffer_reusable,
};

/// Multi-threaded Tokio runtime shared across all JNI calls.
///
/// Worker thread count defaults to Tokio's heuristic (number of
/// logical CPUs) and can be capped for embeddings where the JVM's
/// own thread pools (e.g. Tomcat) compete for the same cores —
/// see [`runtime_worker_threads`].
pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(workers) = runtime_worker_threads() {
        builder.worker_threads(workers);
    }
    builder
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime")
});

const MIN_RUNTIME_WORKERS: usize = 1;
const MAX_RUNTIME_WORKERS: usize = 1024;

static RUNTIME_WORKER_THREADS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

/// Cap on each per-thread sync runtime's blocking pool.
///
/// [`block_on_sync_runtime`] builds ONE current-thread runtime per calling
/// OS thread. A JVM host with a large servlet pool (e.g. 200 Tomcat threads)
/// would otherwise get 200 runtimes each able to spawn Tokio's default 512
/// blocking threads — a worst case approaching 100k threads if handlers use
/// `spawn_blocking` (the multipart extractor's temp-file I/O does). Capping
/// the per-runtime blocking pool bounds that multiplication. Sync dispatch is
/// for small requests; a handler that exceeds the cap simply runs its
/// blocking tasks in batches — no deadlock, because `block_on` keeps driving
/// the runtime. Detached `tokio::spawn` is still unsupported on this path
/// (see [`block_on_sync_runtime`]).
const SYNC_RUNTIME_MAX_BLOCKING_THREADS: usize = 4;

thread_local! {
    static SYNC_RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(SYNC_RUNTIME_MAX_BLOCKING_THREADS)
        .build()
        .expect("failed to create per-thread Tokio runtime");
}

/// Drive a synchronous JNI dispatch on the calling OS thread's
/// current-thread Tokio runtime.
///
/// The request future is driven to completion inside this `block_on`,
/// avoiding shared-runtime enter/scheduler contention on tiny
/// `dispatchBytes` / `dispatchDirect` calls.  Handlers that await their
/// spawned tasks still complete normally, and `spawn_blocking` uses this
/// runtime's blocking pool.  Detached `tokio::spawn` tasks are fragile on
/// this path: a current-thread runtime has no worker threads, so detached
/// tasks only make progress while a later `block_on` runs on the same
/// Java caller thread.  The TLS runtime is dropped when that OS thread
/// exits, cleanly shutting down its per-runtime state.
fn block_on_sync_runtime<F>(future: F) -> F::Output
where
    F: Future,
{
    SYNC_RUNTIME.with(|runtime| runtime.block_on(future))
}

/// Build a `413` wire response when `len` exceeds the configured
/// request-size cap ([`vespera_inprocess::max_request_bytes`]); `None`
/// when within the limit (the default — unlimited).  Lets the buffered
/// JNI entry points reject an oversized request **before** allocating
/// the Rust-side body copy that would otherwise double the Java
/// `byte[]` already resident.
fn oversized_request_wire(len: usize) -> Option<Vec<u8>> {
    if vespera_inprocess::request_exceeds_limit(len) {
        Some(vespera_inprocess::error_wire(
            413,
            &format!(
                "request size {len} bytes exceeds configured maximum of {} bytes",
                vespera_inprocess::max_request_bytes()
            ),
        ))
    } else {
        None
    }
}

/// Clear a pending Java exception (if any) so subsequent JNI calls in
/// the same `with_env` scope are not issued with an exception in flight.
///
/// A failed `GetArrayLength` / region read / `convert_byte_array` (e.g.
/// a `null` array) can leave a pending exception that would poison the
/// follow-up calls (`byte_array_from_slice`, `complete_future_local`,
/// `call_header_consumer`) the dispatch family uses to deliver the wire
/// error response.  Clearing it keeps those calls well-defined.
fn clear_pending_exception(env: &mut jni::Env<'_>) {
    if env.exception_check() {
        env.exception_clear();
    }
}

/// Read a request `byte[]` into an owned buffer, centralizing the
/// ingress contract for every buffered JNI dispatch symbol:
///
/// * `Ok(bytes)` — request body read successfully.
/// * `Err(wire)` — a ready-to-deliver wire response the caller forwards
///   to Java: `413` when the length exceeds the configured cap, `400`
///   when the JNI length query / region read fails.
///
/// On any JNI failure the pending Java exception is cleared first, so
/// the caller can safely make further JNI calls to deliver `Err`.
fn read_request_byte_array(
    env: &mut jni::Env<'_>,
    request_bytes: &JByteArray<'_>,
) -> Result<Vec<u8>, Vec<u8>> {
    let Ok(len) = request_bytes.len(env) else {
        clear_pending_exception(env);
        return Err(vespera_inprocess::error_wire(
            400,
            "invalid input byte array (length query failed)",
        ));
    };
    // Ingress cap: reject an oversized request with 413 BEFORE allocating
    // the Rust-side body copy (the amplification the Java `byte[]` would
    // otherwise double).
    if let Some(err) = oversized_request_wire(len) {
        return Err(err);
    }
    // Read straight into uninitialised capacity — no zero-fill that
    // `get_region` would immediately overwrite.
    let Ok(buf) = crate::jni_buf::read_byte_array_region(env, request_bytes, len) else {
        clear_pending_exception(env);
        return Err(vespera_inprocess::error_wire(
            400,
            "invalid input byte array (JNI conversion failed)",
        ));
    };
    Ok(buf)
}

/// Run a **void** JNI symbol's body under `catch_unwind` so a panic
/// anywhere in it — including the setup that runs *before* the inner
/// dispatch `catch_unwind` (byte-array ingress, global-ref promotion,
/// VM promotion, streaming-buffer checkout, future/header setup) —
/// can never unwind across the `extern "system"` boundary into the JVM.
///
/// A caught panic is swallowed: the inner dispatch guard already does
/// best-effort future/header completion for the common (handler) panic;
/// this outer guard only covers the rare setup-path panic, where no
/// `Env` is available to complete anything anyway.  Matches the
/// whole-body guard already used by `configureRuntime0` /
/// `configureStreaming0`.
fn guard_void_symbol(body: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
}

fn panic_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(500, "panic in Rust engine")
}

fn throw_streaming_abort(env: &mut jni::Env<'_>, header_failed: bool) {
    if header_failed {
        let _ = env.throw_new(
            jni::jni_str!("java/io/IOException"),
            jni::jni_str!("vespera: response header callback failed before body streaming"),
        );
    } else {
        let _ = env.throw_new(
            jni::jni_str!("java/io/IOException"),
            jni::jni_str!("vespera: response body stream aborted after the header was committed"),
        );
    }
}

fn push_unless_header_failed(
    header_failed: &AtomicBool,
    push: &mut impl FnMut(&[u8]) -> std::ops::ControlFlow<()>,
    chunk: &[u8],
) -> std::ops::ControlFlow<()> {
    if header_failed.load(Ordering::SeqCst) {
        std::ops::ControlFlow::Break(())
    } else {
        push(chunk)
    }
}

/// Worker thread count for the shared [`RUNTIME`], resolved once
/// (first hit wins, then fixed for the process lifetime):
///
/// 1. [`set_runtime_worker_threads`] called before the runtime is
///    first used (the `configureRuntime0` JNI hook from
///    `VesperaBridge.init()` lands here)
/// 2. `VESPERA_RUNTIME_WORKERS` environment variable
/// 3. `None` — Tokio's default (number of logical CPUs)
///
/// Values are clamped to `[1, 1024]`.
#[must_use]
pub fn runtime_worker_threads() -> Option<usize> {
    *RUNTIME_WORKER_THREADS.get_or_init(|| {
        std::env::var("VESPERA_RUNTIME_WORKERS")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .map(|v| v.clamp(MIN_RUNTIME_WORKERS, MAX_RUNTIME_WORKERS))
    })
}

/// Override the shared runtime's worker thread count **before the
/// first dispatch**.  Returns `false` when the value was already
/// fixed.  Clamped to `[1, 1024]`.
pub fn set_runtime_worker_threads(workers: usize) -> bool {
    RUNTIME_WORKER_THREADS
        .set(Some(
            workers.clamp(MIN_RUNTIME_WORKERS, MAX_RUNTIME_WORKERS),
        ))
        .is_ok()
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureRuntime0(int) -> void`
///
/// Seeds the shared Tokio runtime's worker thread count **before
/// the first dispatch**.  Values `<= 0` leave the setting
/// untouched (env var / Tokio default applies).  Calls after the
/// configuration is fixed are silently ignored.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureRuntime0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    worker_threads: jint,
) {
    // Defensive `catch_unwind`: this body cannot panic today, but it is
    // an `extern "system"` JNI symbol, so guard it for consistency with
    // the dispatch symbols — an unwind must never cross the FFI boundary.
    let _ = std::panic::catch_unwind(|| {
        if let Ok(workers) = usize::try_from(worker_threads)
            && workers > 0
        {
            let _ = set_runtime_worker_threads(workers);
        }
    });
}

/// Per-chunk buffer size for streaming dispatches.
///
/// Resolved once per process by
/// [`vespera_inprocess::streaming_chunk_bytes`] (default 256 KiB;
/// override via the `VESPERA_STREAMING_CHUNK_BYTES` env var or the
/// `configureStreaming0` JNI setter called from
/// `VesperaBridge.init()`).  Large enough to amortise JNI call
/// overhead, small enough to keep memory bounded for multi-GB
/// streams.  Subsequent calls are a single atomic load.
pub fn streaming_chunk_size() -> usize {
    vespera_inprocess::streaming_chunk_bytes()
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureStreaming0(int, int) -> void`
///
/// Seeds the process-wide streaming configuration **before the
/// first dispatch**.  Values `<= 0` leave the corresponding
/// setting untouched (env var / default applies).  Calls after
/// the configuration is fixed (first dispatch already ran, or a
/// previous call set it) are silently ignored — the JNI side has
/// no use for the failure signal beyond logging, which Java owns.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureStreaming0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    chunk_bytes: jint,
    channel_capacity: jint,
) {
    // Defensive `catch_unwind` — see `configureRuntime0`: keep every JNI
    // `extern "system"` symbol panic-safe even though this body cannot
    // panic with the current setters.
    let _ = std::panic::catch_unwind(|| {
        if let Ok(bytes) = usize::try_from(chunk_bytes)
            && bytes > 0
        {
            let _ = vespera_inprocess::set_streaming_chunk_bytes(bytes);
        }
        if let Ok(slots) = usize::try_from(channel_capacity)
            && slots > 0
        {
            let _ = vespera_inprocess::set_streaming_channel_capacity(slots);
        }
    });
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchBytes(byte[]) -> byte[]`
///
/// **Synchronous** binary wire-format JNI entry point.  Blocks the
/// calling thread until the Rust dispatch completes.  The request-array
/// read AND the dispatch run inside a single `catch_unwind`, so a panic
/// anywhere in that work (including an allocation failure in the ingress
/// read) degrades to a valid wire-format `500` response rather than
/// surfacing as a thrown Java exception.  The only step outside the guard
/// is the final `byte_array_from_slice` that hands the bytes back, itself
/// covered by the `with_env`/`resolve` FFI boundary — so a panic can never
/// unwind across the `extern "system"` boundary into the JVM.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchBytes<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
) -> jbyteArray {
    unowned_env
        .with_env(|env| -> jni::errors::Result<JObject<'local>> {
            // Read + dispatch under ONE guard: a panic in the ingress read
            // (e.g. allocation failure for an unbounded request) now also
            // degrades to a wire `500` instead of a thrown Java exception.
            let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match read_request_byte_array(env, &request_bytes) {
                    Ok(input) => {
                        block_on_sync_runtime(vespera_inprocess::dispatch_from_bytes_async(input))
                    }
                    Err(err_wire) => err_wire,
                }
            }))
            .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

            Ok(env.byte_array_from_slice(&response)?.into())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
        .into_raw()
}

#[path = "jni_impl_direct.rs"]
mod direct;

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchAsync(CompletableFuture<byte[]>, byte[]) -> void`
///
/// **Asynchronous** binary wire-format JNI entry point.  Returns
/// immediately after spawning the dispatch on the shared Tokio
/// runtime.  Completes the supplied `CompletableFuture<byte[]>`
/// from a runtime worker thread once the response is ready.
///
/// Contract (always-complete):
/// - **success** → `future.complete(responseBytes)`
/// - **JNI conversion failure** → `future.complete(error_wire(400, ...))`
/// - **Rust panic / handler crash** → `future.complete(error_wire(500, "panic in Rust engine"))`
///   The future is always completed with a valid wire response —
///   it is never left dangling, even on internal errors.
///
/// # Threading contract (IMPORTANT)
///
/// The future is completed **on a Tokio runtime worker thread**, so any
/// *non-async* `CompletableFuture` continuation (`thenApply`, `thenAccept`,
/// `whenComplete`, …) runs **inline on that worker**.  Callers MUST therefore:
/// - attach heavy / blocking continuations with the `*Async` variants
///   (`thenApplyAsync`, `whenCompleteAsync`, …) on their own executor, and
/// - never re-enter a blocking vespera dispatch (`dispatchBytes` /
///   `dispatchDirect`) from an inline continuation — that nests a `block_on`
///   inside the runtime and degrades to a caught-panic `500`.
///
/// Completing the future off the worker (via `spawn_blocking`) was measured at
/// ~16x the per-dispatch cost (`vespera_inprocess` `benches/dispatch.rs`,
/// group `async_completion_ab`: ~1.5 µs inline vs ~24.5 µs hand-off), so the
/// worker-thread completion is kept and this contract is documented instead —
/// matching how Netty / async HTTP clients complete futures from their I/O
/// threads.  The autoconfigured Spring proxy never selects `ASYNC` (its
/// `SmartDispatchModeResolver` uses DIRECT / SYNC / streaming), so this path is
/// opt-in for callers doing their own `CompletableFuture` composition.
///
/// Cancellation: Java's `future.cancel(true)` does NOT abort the
/// in-flight Rust task in this iteration (defer to follow-up).
/// Java callers may still observe cancellation via `future.isCancelled()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchAsync<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    future_obj: JObject<'local>,
    request_bytes: JByteArray<'local>,
) {
    // The only unrecoverable path is failing to promote the future to a
    // GlobalRef (below): without that ref there is nothing to complete,
    // and a failure there means the JVM is already in trouble.  Every
    // path AFTER the ref exists completes the future, so the
    // always-complete contract holds even on VM-promotion / scheduling
    // failures.
    //
    // JNI-03: the entire body runs under `guard_void_symbol` so a panic
    // in the setup that precedes the inner dispatch guard cannot unwind
    // across this `extern "system"` boundary.
    guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            // On-thread cold paths (oversized, JNI conversion failure, VM
            // promotion / scheduling failure) complete the future via the
            // still-valid LOCAL `future_obj` ref, so only the spawned task
            // needs a `Global` ref (created just before the spawn below) —
            // instead of a second one held solely for these paths.
            let input = match read_request_byte_array(env, &request_bytes) {
                Ok(buf) => buf,
                Err(err) => {
                    let _ = complete_future_local(env, &future_obj, &err);
                    return Ok(());
                }
            };

            // Promote the VM; on the (near-impossible) failure complete the
            // future we already hold so it never dangles.
            let jvm = match env.get_java_vm() {
                Ok(jvm) => jvm,
                Err(e) => {
                    let _ = complete_future_local(
                        env,
                        &future_obj,
                        &vespera_inprocess::error_wire(500, "JNI VM promotion failed"),
                    );
                    return Err(e);
                }
            };

            // The single owning global ref, created only now and moved into
            // the spawned task (which completes the future from a worker
            // thread).  Every on-thread path uses the local `future_obj`
            // instead, so this is the only `Global` ref allocated per call.
            let future_for_task = match env.new_global_ref(&future_obj) {
                Ok(g) => g,
                Err(e) => {
                    let _ = complete_future_local(
                        env,
                        &future_obj,
                        &vespera_inprocess::error_wire(500, "JNI global ref failed"),
                    );
                    return Err(e);
                }
            };

            // A panic in the dispatch future is caught **in place** with
            // `FutureExt::catch_unwind` instead of isolating it in a second
            // `tokio::spawn` task — same panic → 500 wire fallback (preserving
            // always-complete semantics for the Java future), but one fewer
            // task allocation + scheduler hop per async dispatch.  The inner
            // spawn never bought parallelism here (the outer task awaited it
            // immediately), so it was pure overhead.  `AssertUnwindSafe` is
            // sound: a panic drops the half-run dispatch and we return a fresh
            // `error_wire`; the registered `Router` is `Arc`-shared and is not
            // left observably inconsistent.  The outer `catch_unwind` still
            // guards `RUNTIME.spawn` itself so a scheduling failure completes
            // the future (with a 500) instead of leaving the Java caller
            // hanging.
            let scheduled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RUNTIME.spawn(async move {
                    let response = std::panic::AssertUnwindSafe(
                        vespera_inprocess::dispatch_from_bytes_async(input),
                    )
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

                    let _ = with_cached_daemon_env(&jvm, |env| -> jni::errors::Result<()> {
                        complete_future(env, &future_for_task, &response)
                    });
                });
            }));
            if scheduled.is_err() {
                let _ = complete_future_local(
                    env,
                    &future_obj,
                    &vespera_inprocess::error_wire(500, "failed to schedule Rust dispatch"),
                );
            }

            Ok(())
        });
    });
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchStreaming(byte[], OutputStream) -> byte[]`
///
/// **Streaming** JNI entry point.  Drives the dispatch
/// synchronously like [`Java_...dispatchBytes`], but emits the
/// response body chunk-by-chunk by calling `outputStream.write(byte[])`
/// for each chunk axum produces — no full-body materialisation on
/// either the Rust or JVM side.
///
/// Returns the wire-format **header only** (`[u32 BE header_len |
/// header JSON]`) — the body is delivered through the
/// `OutputStream` argument while the dispatch is in flight.
/// Callers (e.g. Spring `StreamingResponseBody`) read the header
/// first to commit the HTTP status + response headers, then
/// continue serving the streamed body bytes.
///
/// Failure modes mirror [`Java_...dispatchBytes`]: malformed wire,
/// version mismatch, no app registered, or Rust panic produce a
/// regular `error_wire(...)` response (header + small body) and
/// the `OutputStream` is **not** written to.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreaming<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
    output_stream: JObject<'local>,
) -> jbyteArray {
    unowned_env
        .with_env(|env| -> jni::errors::Result<JObject<'local>> {
            let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> jni::errors::Result<JObject<'local>> {
                    let input = match read_request_byte_array(env, &request_bytes) {
                        Ok(buf) => buf,
                        Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
                    };

                    // Promote the OutputStream to Global so we can call
                    // .write() from a different attached thread inside
                    // the streaming callback.
                    let stream_global: Global<JObject<'static>> =
                        env.new_global_ref(&output_stream)?;
                    let jvm = env.get_java_vm()?;

                    // One per-thread reusable Java chunk buffer for the whole stream.
                    let (push_buf, push_buf_lease) =
                        checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;

                    let header_bytes =
                        RUNTIME.block_on(vespera_inprocess::dispatch_streaming_async(
                            input,
                            make_push_closure(jvm, stream_global, push_buf),
                        ));
                    mark_streaming_buffer_reusable(push_buf_lease);

                    Ok(env.byte_array_from_slice(&header_bytes)?.into())
                },
            ))
            .unwrap_or_else(|_| Ok(env.byte_array_from_slice(&panic_wire())?.into()))?;

            Ok(response)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
        .into_raw()
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchFullStreaming(byte[], InputStream, OutputStream) -> byte[]`
///
/// **Bidirectional streaming** JNI entry point.  Reads the request
/// body chunk-by-chunk from `inputStream.read(byte[])` and emits
/// response body chunks via `outputStream.write(byte[])` — neither
/// side ever materialises the full body in memory, so 1 GiB
/// uploads with 1 GiB downloads run in O(chunk_size) RAM.
///
/// Returns the wire-format **header only** (`[u32 BE header_len |
/// header JSON]`); the response body was delivered through
/// `outputStream`.
///
/// Wire envelope contract:
/// - `headerBytes` is a wire-format request **without a body**
///   (just the 4-byte length prefix + JSON header).  Send the
///   request body via `inputStream`, not embedded in this buffer.
/// - `inputStream.read(byte[])` semantics: returns `-1` on EOF,
///   `0` for an empty read (will be retried), or `>0` for the
///   number of bytes read into the supplied buffer.
///
/// Failure modes mirror [`Java_...dispatchStreaming`]: malformed
/// wire / unknown version / no app / Rust panic produce a normal
/// `error_wire(...)` response in the returned bytes and neither
/// stream is touched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchFullStreaming<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    header_bytes: JByteArray<'local>,
    input_stream: JObject<'local>,
    output_stream: JObject<'local>,
) -> jbyteArray {
    unowned_env
        .with_env(|env| -> jni::errors::Result<JObject<'local>> {
            let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> jni::errors::Result<JObject<'local>> {
                    // Read the header byte[] through the shared ingress contract
                    // (length cap honoured + pending-exception scrub on failure)
                    // rather than a raw `convert_byte_array`, so an oversized header
                    // byte[] is rejected before a full Rust-side copy — parity with
                    // the buffered dispatch symbols.
                    let header_input = match read_request_byte_array(env, &header_bytes) {
                        Ok(buf) => buf,
                        Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
                    };

                    let input_global: Global<JObject<'static>> =
                        env.new_global_ref(&input_stream)?;
                    // A second InputStream ref for the post-response close — the
                    // first is moved into the pull closure (a `Global` is not
                    // `Clone`); both are independent GC roots to the same stream.
                    let input_for_close: Global<JObject<'static>> =
                        env.new_global_ref(&input_stream)?;
                    let output_global: Global<JObject<'static>> =
                        env.new_global_ref(&output_stream)?;
                    let jvm = env.get_java_vm()?;

                    // Pull and push run concurrently on different threads, so each
                    // direction checks out its own per-thread cached buffer (the
                    // pull lease is released for us if the push checkout fails).
                    let PullPushBuffers {
                        pull_buf,
                        pull_buf_lease,
                        push_buf,
                        push_buf_lease,
                    } = checkout_pull_push_buffers(env)?;

                    // Closures capture clones of the JavaVM and Globals;
                    // both types are Send+Sync.
                    let pull_jvm = jvm.clone();
                    let pull_global = input_global;
                    let close_jvm = jvm.clone();
                    let push_jvm = jvm;
                    let push_global = output_global;

                    let header_response = RUNTIME.block_on(
                        vespera_inprocess::dispatch_bidirectional_streaming_closing(
                            header_input,
                            // Pull request body chunks from Java InputStream.
                            // Runs on a tokio blocking thread (spawn_blocking
                            // inside dispatch_bidirectional_streaming).
                            make_pull_closure(pull_jvm, pull_global, pull_buf),
                            // Push response body chunks to Java OutputStream.
                            // Runs on the tokio worker driving the dispatch.
                            make_push_closure(push_jvm, push_global, push_buf),
                            // Close the InputStream once the response is fully
                            // streamed, so a producer parked in a blocking read is
                            // unblocked and the dispatch cannot hang on a stuck
                            // upload that never reaches EOF.
                            move || {
                                let _ = with_cached_daemon_env(&close_jvm, |env| {
                                    close_input_stream(env, &input_for_close)
                                });
                            },
                        ),
                    );
                    mark_streaming_buffer_reusable(pull_buf_lease);
                    mark_streaming_buffer_reusable(push_buf_lease);

                    Ok(env.byte_array_from_slice(&header_response)?.into())
                },
            ))
            .unwrap_or_else(|_| Ok(env.byte_array_from_slice(&panic_wire())?.into()))?;

            Ok(response)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
        .into_raw()
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchStreamingWithHeader(byte[], Consumer<byte[]>, OutputStream) -> void`
///
/// Same as [`Java_...dispatchStreaming`] but emits the wire-format
/// response header via `headerConsumer.accept(byte[])` **before**
/// the first body byte reaches `outputStream`.  This lets
/// Spring-style `HttpServletResponse` controllers commit status
/// and headers while the response is still uncommitted.
///
/// `headerConsumer` is invoked exactly once on every code path
/// (success or error); the bytes are a normal wire-format header
/// (length-prefixed JSON).  On error `outputStream` is not
/// touched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreamingWithHeader<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
    header_consumer: JObject<'local>,
    output_stream: JObject<'local>,
) {
    // JNI-03: whole-body panic guard (see `guard_void_symbol`).
    guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            let input = match read_request_byte_array(env, &request_bytes) {
                Ok(buf) => buf,
                Err(err) => {
                    let _ = call_header_consumer(env, &env.new_global_ref(&header_consumer)?, &err);
                    return Ok(());
                }
            };

            let header_global: Global<JObject<'static>> = env.new_global_ref(&header_consumer)?;
            let stream_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
            let jvm = env.get_java_vm()?;

            // One per-thread reusable Java chunk buffer for the whole stream.
            let (push_buf, push_buf_lease) =
                checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;

            // Panic safety: catch_unwind absorbs Rust panics so the JVM
            // never sees an unwinding stack across the FFI boundary.
            // `header_sent` records whether the header callback fired; if a
            // panic unwinds BEFORE it does (e.g. the axum handler panicked
            // inside dispatch, before status/headers are produced), we fire
            // the consumer once with a 500 header below so the documented
            // "header consumer invoked exactly once on every code path"
            // contract holds and the Java caller is not left hanging.  A
            // panic AFTER the header fired leaves Spring's response partially
            // committed — unrecoverable, but the contract is already met.
            let header_sent = Arc::new(AtomicBool::new(false));
            let header_failed = Arc::new(AtomicBool::new(false));
            let header_sent_cb = Arc::clone(&header_sent);
            let header_failed_cb = Arc::clone(&header_failed);
            let header_failed_push = Arc::clone(&header_failed);
            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let header_for_cb = header_global;
                let jvm_for_cb = jvm.clone();
                let mut push = make_push_closure(jvm, stream_global, push_buf);
                RUNTIME.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
                    input,
                    |header_bytes: &[u8]| {
                        if with_cached_daemon_env(
                            &jvm_for_cb,
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                call_header_consumer(env, &header_for_cb, header_bytes)
                            },
                        )
                        .is_ok()
                        {
                            header_sent_cb.store(true, Ordering::SeqCst);
                        } else {
                            header_failed_cb.store(true, Ordering::SeqCst);
                        }
                    },
                    move |chunk: &[u8]| {
                        push_unless_header_failed(&header_failed_push, &mut push, chunk)
                    },
                ))
            }));
            match panic_result {
                Ok(outcome) => {
                    mark_streaming_buffer_reusable(push_buf_lease);
                    let failed_header = header_failed.load(Ordering::SeqCst);
                    // The header was already committed via the consumer, so a
                    // failure that aborts the body mid-stream can no longer
                    // change the status.  Surface it as a thrown IOException so
                    // the servlet container aborts the response instead of
                    // finishing cleanly over a truncated body — the host
                    // otherwise cannot tell a short stream from a complete one.
                    if failed_header
                        || matches!(
                            outcome,
                            vespera_inprocess::StreamOutcome::BodyError
                                | vespera_inprocess::StreamOutcome::SinkStopped
                        )
                    {
                        throw_streaming_abort(env, failed_header);
                    }
                }
                Err(_) => {
                    if !header_sent.load(Ordering::SeqCst)
                        && let Ok(fallback) = env.new_global_ref(&header_consumer)
                    {
                        let err = panic_wire();
                        let _ = call_header_consumer(env, &fallback, &err);
                    }
                }
            }

            Ok(())
        });
    });
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchFullStreamingWithHeader(byte[], Consumer<byte[]>, InputStream, OutputStream) -> void`
///
/// Bidirectional streaming with the same header-callback contract
/// as [`Java_...dispatchStreamingWithHeader`].  Request body
/// pulled from `inputStream`, response header emitted via
/// `headerConsumer.accept(byte[])` once axum produces status +
/// headers, then response body chunks streamed to `outputStream`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchFullStreamingWithHeader<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    header_bytes_in: JByteArray<'local>,
    header_consumer: JObject<'local>,
    input_stream: JObject<'local>,
    output_stream: JObject<'local>,
) {
    // JNI-03: whole-body panic guard (see `guard_void_symbol`).
    guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            // Read the header byte[] through the shared ingress contract
            // (length cap honoured + pending-exception scrub on failure)
            // rather than a raw `convert_byte_array`, so an oversized header
            // byte[] is rejected before a full Rust-side copy — parity with
            // the buffered dispatch symbols.  The wire error is delivered
            // through the header callback (this is a void symbol).
            let header_input = match read_request_byte_array(env, &header_bytes_in) {
                Ok(buf) => buf,
                Err(err) => {
                    let _ = call_header_consumer(env, &env.new_global_ref(&header_consumer)?, &err);
                    return Ok(());
                }
            };

            let header_global: Global<JObject<'static>> = env.new_global_ref(&header_consumer)?;
            let input_global: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
            // Second InputStream ref for the post-response close (the first is
            // moved into the pull closure; `Global` is not `Clone`).
            let input_for_close: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
            let output_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
            let jvm = env.get_java_vm()?;

            // Pull and push run concurrently on different threads (the pull
            // lease is released for us if the push checkout fails).
            let PullPushBuffers {
                pull_buf,
                pull_buf_lease,
                push_buf,
                push_buf_lease,
            } = checkout_pull_push_buffers(env)?;

            let pull_jvm = jvm.clone();
            let pull_global = input_global;
            let push_jvm = jvm.clone();
            let push_global = output_global;
            let close_jvm = jvm.clone();
            let header_jvm = jvm;
            let header_for_cb = header_global;

            // See dispatchStreamingWithHeader: `header_sent` lets us honour
            // the "header consumer invoked exactly once on every code path"
            // contract — if a panic unwinds before the header callback fires
            // (e.g. the handler panicked before producing status/headers),
            // we fire the consumer once with a 500 below instead of leaving
            // the Java caller hanging.
            let header_sent = Arc::new(AtomicBool::new(false));
            let header_failed = Arc::new(AtomicBool::new(false));
            let header_sent_cb = Arc::clone(&header_sent);
            let header_failed_cb = Arc::clone(&header_failed);
            let header_failed_push = Arc::clone(&header_failed);
            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut push = make_push_closure(push_jvm, push_global, push_buf);
                RUNTIME.block_on(
                    vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                        header_input,
                        make_pull_closure(pull_jvm, pull_global, pull_buf),
                        move |chunk: &[u8]| {
                            push_unless_header_failed(&header_failed_push, &mut push, chunk)
                        },
                        |header_bytes: &[u8]| {
                            if with_cached_daemon_env(
                                &header_jvm,
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    call_header_consumer(env, &header_for_cb, header_bytes)
                                },
                            )
                            .is_ok()
                            {
                                header_sent_cb.store(true, Ordering::SeqCst);
                            } else {
                                header_failed_cb.store(true, Ordering::SeqCst);
                            }
                        },
                        // Close the InputStream once the response is fully
                        // streamed, to unblock a producer parked in a blocking
                        // read so the dispatch cannot hang on a stuck upload.
                        move || {
                            let _ = with_cached_daemon_env(&close_jvm, |env| {
                                close_input_stream(env, &input_for_close)
                            });
                        },
                    ),
                )
            }));
            match panic_result {
                Ok(outcome) => {
                    mark_streaming_buffer_reusable(pull_buf_lease);
                    mark_streaming_buffer_reusable(push_buf_lease);
                    let failed_header = header_failed.load(Ordering::SeqCst);
                    // Header already committed: a post-header body abort can no
                    // longer change the status, so throw IOException to make the
                    // servlet container abort the response rather than finish
                    // cleanly over a truncated body.
                    if failed_header
                        || matches!(
                            outcome,
                            vespera_inprocess::StreamOutcome::BodyError
                                | vespera_inprocess::StreamOutcome::SinkStopped
                        )
                    {
                        throw_streaming_abort(env, failed_header);
                    }
                }
                Err(_) => {
                    if !header_sent.load(Ordering::SeqCst)
                        && let Ok(fallback) = env.new_global_ref(&header_consumer)
                    {
                        let err = panic_wire();
                        let _ = call_header_consumer(env, &fallback, &err);
                    }
                }
            }

            Ok(())
        });
    });
}

#[cfg(test)]
#[path = "jni_impl_runtime_config_tests.rs"]
mod runtime_config_tests;

#[cfg(test)]
#[path = "jni_impl_streaming_abort_tests.rs"]
mod streaming_abort_tests;
