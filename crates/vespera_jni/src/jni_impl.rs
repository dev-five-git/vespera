use std::{
    cell::RefCell,
    future::Future,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::jbyteArray;

use crate::daemon_env::with_cached_daemon_env;
use crate::streaming_closures::{
    close_input_stream, complete_future, complete_future_local, make_pull_closure,
    make_push_closure,
};

// Per-thread reusable Java chunk buffers for the streaming paths live in
// a sidecar module to keep this file within the 1000-line source cap.
#[path = "jni_impl_streaming_buffer.rs"]
pub mod streaming_buffer;
use streaming_buffer::{PullPushBuffers, mark_streaming_buffer_reusable};

// Runtime / streaming configuration JNI hooks (seeded from
// `VesperaBridge.init()` before the first dispatch) live in a sidecar
// module so this file stays focused on the per-request dispatch symbols.
#[path = "jni_impl_config.rs"]
mod config;
pub use config::{runtime_worker_threads, streaming_chunk_size};

/// Multi-threaded Tokio runtime shared across all JNI calls.
///
/// Worker thread count defaults to Tokio's heuristic (number of
/// logical CPUs) and can be capped for embeddings where the JVM's
/// own thread pools (e.g. Tomcat) compete for the same cores —
/// see [`runtime_worker_threads`].
static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();

pub fn runtime() -> Option<&'static tokio::runtime::Runtime> {
    RUNTIME
        .get_or_init(|| {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            if let Some(workers) = runtime_worker_threads() {
                builder.worker_threads(workers);
            }
            builder.enable_all().build().ok()
        })
        .as_ref()
}

pub fn runtime_unavailable_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(500, "failed to create Tokio runtime")
}

fn block_on_shared_runtime<F>(future: F) -> Option<F::Output>
where
    F: Future,
{
    runtime().map(|runtime| runtime.block_on(future))
}

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
    static SYNC_RUNTIME: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
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
pub fn block_on_sync_runtime<F>(future: F) -> Option<F::Output>
where
    F: Future,
{
    SYNC_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        if runtime.is_none() {
            *runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(SYNC_RUNTIME_MAX_BLOCKING_THREADS)
                .build()
                .ok();
        }
        runtime.as_ref().map(|runtime| runtime.block_on(future))
    })
}

/// Clear a pending Java exception (if any) so subsequent JNI calls in
/// the same `with_env` scope are not issued with an exception in flight.
///
/// A failed `GetArrayLength` / region read / `convert_byte_array` (e.g.
/// a `null` array) can leave a pending exception that would poison the
/// follow-up calls (`byte_array_from_slice`, `complete_future_local`,
/// `call_header_consumer`) the dispatch family uses to deliver the wire
/// error response.  Clearing it keeps those calls well-defined.
pub fn clear_pending_exception(env: &mut jni::Env<'_>) {
    if env.exception_check() {
        env.exception_clear();
    }
}

/// Canonical `400` wire response for an invalid JNI input byte array.
/// `detail` is rendered in the parenthesised tail (`null`,
/// `length query failed`, `JNI conversion failed`) so the prefix
/// `"invalid input byte array ("` stays in one place across the three
/// ingress failure modes [`read_request_byte_array`] produces.
///
/// Same drift-prevention pattern as `vespera_inprocess`'s
/// `BODY_STREAM_ERROR_MSG` / `HEADER_TOO_LARGE_MSG` / `invalid_request_err`
/// — a future wording change (e.g. "invalid JNI input byte array") that
/// touched only one of the three call sites would silently desync the
/// wire `400` body for one of the failure modes; centralising the prefix
/// makes that impossible.
///
/// The format string is byte-identical to the prior inlined literals,
/// so wire `400` bodies for the three failure modes are unchanged.
fn invalid_input_array_err(detail: &str) -> Vec<u8> {
    vespera_inprocess::error_wire(400, &format!("invalid input byte array ({detail})"))
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
pub fn read_request_byte_array(
    env: &mut jni::Env<'_>,
    request_bytes: &JByteArray<'_>,
) -> Result<Vec<u8>, Vec<u8>> {
    if request_bytes.is_null() {
        return Err(invalid_input_array_err("null"));
    }
    let Ok(len) = request_bytes.len(env) else {
        clear_pending_exception(env);
        return Err(invalid_input_array_err("length query failed"));
    };
    // Ingress cap: reject an oversized request with 413 BEFORE allocating
    // the Rust-side body copy (the amplification the Java `byte[]` would
    // otherwise double).
    if let Some(err) = vespera_inprocess::check_ingress_cap(len) {
        return Err(err);
    }
    // Read straight into uninitialised capacity — no zero-fill that
    // `get_region` would immediately overwrite.  The reservation inside
    // `read_byte_array_region` is fallible, so an oversized request that
    // slipped past a loose / unlimited ingress cap reports `NoMemory` and
    // degrades to a wire `413` instead of aborting the host JVM.
    match crate::jni_buf::read_byte_array_region(env, request_bytes, len) {
        Ok(buf) => Ok(buf),
        Err(jni::errors::Error::JniCall(jni::errors::JniError::NoMemory)) => {
            // try_reserve failed before any JNI call, so there is no pending
            // exception to scrub here.
            Err(vespera_inprocess::error_wire(
                413,
                &format!("request body of {len} bytes could not be allocated"),
            ))
        }
        Err(_) => {
            clear_pending_exception(env);
            Err(invalid_input_array_err("JNI conversion failed"))
        }
    }
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
pub fn guard_void_symbol(body: impl FnOnce()) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err()
}

pub fn panic_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(500, "panic in Rust engine")
}

pub fn byte_array_or_empty<'local>(
    env: &mut jni::Env<'local>,
    bytes: &[u8],
) -> jni::errors::Result<JObject<'local>> {
    env.byte_array_from_slice(bytes)
        .map(Into::into)
        .or_else(|_| {
            clear_pending_exception(env);
            // Last-resort OOM-of-OOM fallback: an empty byte[] signals that wire
            // framing itself was unavailable, without throwing a Java exception or
            // returning null from the outer panic landing pad.
            env.new_byte_array(0).map(Into::into)
        })
}

#[path = "jni_impl_support.rs"]
pub mod support;
use support::{setup_full_stream, setup_stream};

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
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                // Read + dispatch under ONE guard: a panic in the ingress read
                // (e.g. allocation failure for an unbounded request) now also
                // degrades to a wire `500` instead of a thrown Java exception.
                let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match read_request_byte_array(env, &request_bytes) {
                        Ok(input) => block_on_sync_runtime(
                            vespera_inprocess::dispatch_from_bytes_async(input),
                        )
                        .unwrap_or_else(runtime_unavailable_wire),
                        Err(err_wire) => err_wire,
                    }
                }))
                .unwrap_or_else(|_| panic_wire());

                Ok(env.byte_array_from_slice(&response)?.into())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }));
    guarded.unwrap_or_else(|_| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                byte_array_or_empty(env, &panic_wire())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    })
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
/// - never re-enter a blocking vespera dispatch from an inline continuation —
///   that nests a `block_on` inside the runtime and degrades to a caught-panic
///   `500`. This applies to EVERY blocking JNI entry point, not just
///   `dispatchBytes` / `dispatchDirect`: the streaming symbols
///   (`dispatchStreaming`, `dispatchFullStreaming`, and their `*WithHeader`
///   variants) also `RUNTIME.block_on(...)` and are *more* damaging to
///   re-enter because they hold a worker across the entire response/request
///   stream.
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
    let future_notified = Arc::new(AtomicBool::new(false));
    let future_notified_body = Arc::clone(&future_notified);
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
    let panicked = guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            if future_obj.is_null() {
                let _ = env.throw_new(
                    jni::jni_str!("java/lang/IllegalArgumentException"),
                    jni::jni_str!("future must not be null"),
                );
                future_notified_body.store(true, Ordering::Release);
                return Ok(());
            }
            // On-thread cold paths (oversized, JNI conversion failure, VM
            // promotion / scheduling failure) complete the future via the
            // still-valid LOCAL `future_obj` ref, so only the spawned task
            // needs a `Global` ref (created just before the spawn below) —
            // instead of a second one held solely for these paths.
            let input = match read_request_byte_array(env, &request_bytes) {
                Ok(buf) => buf,
                Err(err) => {
                    let _ = complete_future_local(env, &future_obj, &err);
                    future_notified_body.store(true, Ordering::Release);
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
                    future_notified_body.store(true, Ordering::Release);
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
                    future_notified_body.store(true, Ordering::Release);
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
                let Some(runtime) = runtime() else {
                    return false;
                };
                runtime.spawn(async move {
                    let response = std::panic::AssertUnwindSafe(
                        vespera_inprocess::dispatch_from_bytes_async(input),
                    )
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| panic_wire());

                    // ALWAYS-COMPLETE CONTRACT: the Java CompletableFuture must
                    // resolve on every path or `dispatchAsync` callers hang
                    // forever.  The cached-daemon completion can fail (daemon
                    // attach during VM shutdown, or an OOM allocating the
                    // response byte[]); on failure make a best-effort second
                    // attempt with a tiny error payload (far less likely to OOM
                    // than the full response) so the future still resolves with
                    // an error rather than never.  If even that fails the JVM is
                    // unrecoverable and nothing could complete it.
                    let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        with_cached_daemon_env(&jvm, |env| -> jni::errors::Result<()> {
                            complete_future(env, &future_for_task, &response)
                        })
                    }));
                    if !matches!(completed, Ok(Ok(()))) {
                        let _ = with_cached_daemon_env(&jvm, |env| -> jni::errors::Result<()> {
                            complete_future(
                                env,
                                &future_for_task,
                                &vespera_inprocess::error_wire(500, "async completion failed"),
                            )
                        });
                    }
                });
                true
            }));
            if matches!(scheduled, Ok(true)) {
                future_notified_body.store(true, Ordering::Release);
            } else {
                let _ = complete_future_local(
                    env,
                    &future_obj,
                    &vespera_inprocess::error_wire(500, "failed to schedule Rust dispatch"),
                );
                future_notified_body.store(true, Ordering::Release);
            }

            Ok(())
        });
    });
    if panicked && !future_notified.load(Ordering::Acquire) && !future_obj.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            complete_future_local(env, &future_obj, &panic_wire())
        });
    }
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
/// Failure modes mirror [`Java_...dispatchBytes`]: a **pre-streaming**
/// failure (malformed wire, version mismatch, no app registered, or a panic
/// before the first body frame) produces a regular `error_wire(...)` response
/// (header + small body) and the `OutputStream` is **not** written to.  A
/// failure that occurs **after** the first body frame (the host
/// `OutputStream` erroring mid-drain, or a body-stream error) may leave
/// partial bytes already written to the `OutputStream`; it is still reported
/// as a `500` `error_wire(...)` header return, so the caller must treat a
/// `5xx` header returned after streaming has begun as a truncated response.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreaming<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
    output_stream: JObject<'local>,
) -> jbyteArray {
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> jni::errors::Result<JObject<'local>> {
                        if output_stream.is_null() {
                            return Ok(env
                                .byte_array_from_slice(&vespera_inprocess::error_wire(
                                    400,
                                    "outputStream must not be null",
                                ))?
                                .into());
                        }
                        let input = match read_request_byte_array(env, &request_bytes) {
                            Ok(buf) => buf,
                            Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
                        };

                        // Promote the OutputStream to a Global (so the streaming
                        // callback can call .write() from a daemon-attached worker
                        // thread), grab the VM, and check out the per-thread push
                        // chunk buffer.  On ANY setup failure (rare, OOM-driven) the
                        // previous bare `?` returned an ignored `Err` from `with_env`
                        // → `resolve::<ThrowRuntimeExAndDefault>` threw a Java
                        // exception + returned `null`, breaking the "every failure is
                        // a valid wire response" contract.  Return a `500` wire
                        // response instead so the Java decoder is never handed `null`.
                        let Ok((stream_global, jvm, push_buf, push_buf_lease)) =
                            setup_stream(env, &output_stream)
                        else {
                            clear_pending_exception(env);
                            return Ok(env
                                .byte_array_from_slice(&vespera_inprocess::error_wire(
                                    500,
                                    "JNI streaming setup failed",
                                ))?
                                .into());
                        };

                        let header_bytes =
                            block_on_shared_runtime(vespera_inprocess::dispatch_streaming_async(
                                input,
                                make_push_closure(jvm, stream_global, push_buf),
                            ))
                            .unwrap_or_else(runtime_unavailable_wire);
                        mark_streaming_buffer_reusable(push_buf_lease);

                        Ok(env.byte_array_from_slice(&header_bytes)?.into())
                    },
                ))
                .unwrap_or_else(|_| byte_array_or_empty(env, &panic_wire()))?;

                Ok(response)
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }));
    guarded.unwrap_or_else(|_| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                byte_array_or_empty(env, &panic_wire())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    })
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
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> jni::errors::Result<JObject<'local>> {
                        if input_stream.is_null() || output_stream.is_null() {
                            return Ok(env
                                .byte_array_from_slice(&vespera_inprocess::error_wire(
                                    400,
                                    "inputStream and outputStream must not be null",
                                ))?
                                .into());
                        }
                        // Read the header byte[] through the shared ingress contract
                        // (length cap honoured + pending-exception scrub on failure)
                        // rather than a raw `convert_byte_array`, so an oversized header
                        // byte[] is rejected before a full Rust-side copy — parity with
                        // the buffered dispatch symbols.
                        let header_input = match read_request_byte_array(env, &header_bytes) {
                            Ok(buf) => buf,
                            Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
                        };

                        // Promote the input/output refs (+ a second input ref for the
                        // post-response close, since `Global` is not `Clone`), grab the
                        // VM, and check out both per-thread chunk buffers.  On ANY setup
                        // failure (rare, OOM-driven) the previous bare `?` surfaced to
                        // Java as a thrown exception + `null` return; return a `500` wire
                        // response instead so the decoder is never handed `null`.  A
                        // half-acquired buffer pair cannot leak a lease (see
                        // `setup_full_stream` / `checkout_pull_push_buffers`).
                        let Ok((input_global, output_global, jvm, buffers)) =
                            setup_full_stream(env, &input_stream, &output_stream)
                        else {
                            clear_pending_exception(env);
                            return Ok(env
                                .byte_array_from_slice(&vespera_inprocess::error_wire(
                                    500,
                                    "JNI streaming setup failed",
                                ))?
                                .into());
                        };
                        let PullPushBuffers {
                            pull_buf,
                            pull_buf_lease,
                            push_buf,
                            push_buf_lease,
                        } = buffers;

                        // Closures capture clones of the JavaVM and Globals;
                        // both types are Send+Sync.
                        let pull_jvm = jvm.clone();
                        let pull_global = Arc::clone(&input_global);
                        let close_jvm = jvm.clone();
                        let input_for_close = input_global;
                        let push_jvm = jvm;
                        let push_global = output_global;

                        let header_response = block_on_shared_runtime(
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
                        )
                        .unwrap_or_else(runtime_unavailable_wire);
                        mark_streaming_buffer_reusable(pull_buf_lease);
                        mark_streaming_buffer_reusable(push_buf_lease);

                        Ok(env.byte_array_from_slice(&header_response)?.into())
                    },
                ))
                .unwrap_or_else(|_| byte_array_or_empty(env, &panic_wire()))?;

                Ok(response)
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }));
    guarded.unwrap_or_else(|_| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                byte_array_or_empty(env, &panic_wire())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    })
}

#[cfg(test)]
#[path = "jni_impl_runtime_config_tests.rs"]
mod runtime_config_tests;

#[cfg(test)]
#[path = "jni_impl_streaming_abort_tests.rs"]
mod streaming_abort_tests;
