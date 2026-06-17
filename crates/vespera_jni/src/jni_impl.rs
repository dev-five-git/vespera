use std::{future::Future, sync::LazyLock};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{Global, JByteArray, JByteBuffer, JClass, JObject};
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

thread_local! {
    static SYNC_RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
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
/// calling thread until the Rust dispatch completes.  Wraps the
/// entire pipeline in `catch_unwind` so a panic anywhere produces
/// a valid wire-format `500` response with a plain-text body —
/// JVM never sees an unwinding stack across the FFI boundary.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchBytes<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
) -> jbyteArray {
    unowned_env
        .with_env(|env| -> jni::errors::Result<JObject<'local>> {
            let input = match read_request_byte_array(env, &request_bytes) {
                Ok(buf) => buf,
                Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
            };

            let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on_sync_runtime(vespera_inprocess::dispatch_from_bytes_async(input))
            }))
            .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

            Ok(env.byte_array_from_slice(&response)?.into())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
        .into_raw()
}

/// Sentinel for [`Java_..._dispatchDirect`]: the response (or its
/// required size) cannot be represented in the `jint` return value
/// (> `i32::MAX` bytes).
///
/// `jint::MIN` is the only value the `-(required_size)` protocol can
/// never produce: `required_size <= i32::MAX`, so the most negative
/// legitimate return is `-(i32::MAX) == jint::MIN + 1`.
const DIRECT_UNREPRESENTABLE: jint = jint::MIN;

// Compile-time proof that the sentinel cannot collide with any
// legitimate `-(required_size)` value.
const _: () = assert!(DIRECT_UNREPRESENTABLE < -i32::MAX);

/// Copy `response` into the caller's direct out buffer.
///
/// Returns:
/// * `>= 0` — bytes written (`response` fit entirely)
/// * `< 0`  — `-(required_size)`: nothing written, caller must retry
///   with a buffer of at least `required_size` bytes
/// * [`DIRECT_UNREPRESENTABLE`] — response exceeds `i32::MAX` bytes
///   and cannot be expressed in the return-code protocol
///
/// # Safety contract (upheld by the caller)
///
/// `out_addr` must point to a writable region of at least `out_cap`
/// bytes that stays valid for the duration of this call (a JNI
/// direct buffer pinned by the live `JByteBuffer` local ref).
/// Whether `[a0, a0+a_len)` and `[b0, b0+b_len)` overlap (addresses as
/// `usize`).  Used to reject aliasing `in_buf` / `out_buf` direct-buffer
/// ranges in [`Java_..._dispatchDirect0`] before creating a shared `&[u8]`
/// and an exclusive `&mut [u8]` over them (SEC-1).  `saturating_add`
/// keeps the bound arithmetic panic-free for any address.
fn ranges_overlap(a0: usize, a_len: usize, b0: usize, b_len: usize) -> bool {
    let a1 = a0.saturating_add(a_len);
    let b1 = b0.saturating_add(b_len);
    a0 < b1 && b0 < a1
}

fn write_response_to_out(out_addr: *mut u8, out_cap: usize, response: &[u8]) -> jint {
    if response.len() <= out_cap {
        // SAFETY: `response.len() <= out_cap` and the caller
        // guarantees `out_addr..out_addr+out_cap` is writable.
        // Source and destination cannot overlap: `response` is a
        // Rust-owned Vec, the destination is a Java direct buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(response.as_ptr(), out_addr, response.len());
        }
        // Java buffer capacities are jint-bounded, so len <= cap
        // always fits i32.
        jint::try_from(response.len()).unwrap_or(DIRECT_UNREPRESENTABLE)
    } else {
        jint::try_from(response.len()).map_or(DIRECT_UNREPRESENTABLE, |required| -required)
    }
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchDirect0(ByteBuffer, int, ByteBuffer) -> int`
/// (private native; the public Java wrapper `dispatchDirect` validates
/// buffer directness before crossing JNI)
///
/// **Direct-buffer** synchronous dispatch — the zero-JNI-region-copy
/// sibling of [`Java_...dispatchBytes`].
///
/// Contract (mirrored in the Java wrapper's javadoc):
/// * `in_buf` / `out_buf` MUST be **direct** `ByteBuffer`s.  The
///   Java wrapper enforces this before crossing JNI; non-direct
///   buffers reaching this symbol produce a thrown
///   `RuntimeException` (the jni crate surfaces a null direct
///   address as `Err`).
/// * The wire request is read from `in_buf[0..in_len]` — explicit
///   `in_len`, **never** the buffer's position/limit (eliminates
///   the classic "forgot to flip()" corruption).
/// * Return `>= 0`: a complete wire response was written to
///   `out_buf[0..n]`.
/// * Return `< 0`: `-(required_size)` — the response did not fit.
///   `out_buf` contents are **undefined** (a prefix may have been
///   written).  `required_size` is exact, but retrying re-runs the
///   dispatch, so the Java side only auto-retries idempotent
///   methods.
/// * `Integer.MIN_VALUE`: response size exceeds `i32::MAX`.
///
/// Compared with `dispatchBytes`, this path removes BOTH JNI
/// region copies (Java `byte[]` ↔ Rust), the per-call Java heap
/// array allocations, AND — via
/// [`vespera_inprocess::dispatch_into_async_borrowed`] — the
/// intermediate response `Vec` AND the request-side input copy: the
/// wire header is parsed **in place** from the borrowed `in_buf`, and
/// only a non-empty request body is copied into an owned `Bytes`
/// (axum's `Body` requires `'static` ownership), so a bodyless `GET`
/// copies nothing on the request side.  On the success path the wire
/// header and each body frame are written straight into `out_buf`.
/// `422` responses are materialised internally to preserve
/// `validation_errors` hoisting.
///
/// # Safety invariants (comment-locked)
///
/// 1. `in_buf` / `out_buf` stay rooted as live local refs for the
///    whole call — HotSpot neither moves nor frees the backing
///    memory of a direct buffer while its object is reachable.
/// 2. The raw addresses derived from them are used **only within
///    this function body** — never captured by closures, spawned
///    tasks, or returned structs.
/// 3. The input is read through a **borrowed** slice for the duration
///    of the synchronous `block_on` (no `Vec` copy).  Invariant 1
///    keeps the backing memory valid throughout and the borrow never
///    escapes the `block_on`, so nothing borrowed from the buffer
///    outlives the call.
/// 4. `in_buf` and `out_buf` are proven **non-overlapping** (SEC-1)
///    before the shared `&[u8]` / exclusive `&mut [u8]` are created, so
///    they never alias the same memory; and `out_buf` is **writable**
///    (the Java wrapper rejects read-only buffers — SEC-2), so the
///    `&mut [u8]` write target is valid.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchDirect0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    in_buf: JByteBuffer<'local>,
    in_len: jint,
    out_buf: JByteBuffer<'local>,
) -> jint {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jint> {
            // Err here (null address ⇒ heap buffer, or JVM trouble)
            // is thrown as RuntimeException via the resolve below —
            // defense in depth behind the Java-side isDirect() check.
            let in_addr = env.get_direct_buffer_address(&in_buf)?;
            let in_cap = env.get_direct_buffer_capacity(&in_buf)?;
            let out_addr = env.get_direct_buffer_address(&out_buf)?;
            let out_cap = env.get_direct_buffer_capacity(&out_buf)?;

            // Validate in_len against the buffer's real capacity —
            // all failures still produce a valid wire response in
            // `out_buf`, per the dispatch* family contract.
            let in_len = match usize::try_from(in_len) {
                Ok(len) if len <= in_cap => len,
                _ => {
                    let err = vespera_inprocess::error_wire(
                        400,
                        "invalid in_len (negative or exceeds buffer capacity)",
                    );
                    return Ok(write_response_to_out(out_addr, out_cap, &err));
                }
            };

            // SEC-1: reject overlapping `in_buf` / `out_buf` ranges.
            // Below we create a shared `&[u8]` over the input and an
            // exclusive `&mut [u8]` over the output; if they alias the
            // same direct-buffer memory (the caller passed the same
            // buffer, or overlapping `slice()`/`duplicate()` views) that
            // is instant UB.  The Java wrapper cannot detect this (it has
            // no native address), so the check lives here.  `out_buf` is
            // writable by the wrapper's `isReadOnly()` guard (SEC-2), so
            // writing the error response into it is sound.
            if ranges_overlap(in_addr as usize, in_len, out_addr as usize, out_cap) {
                let err = vespera_inprocess::error_wire(
                    400,
                    "in_buf and out_buf must not overlap (aliasing would be undefined behavior)",
                );
                return Ok(write_response_to_out(out_addr, out_cap, &err));
            }

            let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // SAFETY: invariants 1–3 above.  `in_addr..in_addr+in_len`
                // (`in_len <= in_cap`) is a readable region and
                // `out_addr..out_addr+out_cap` a writable region, both of
                // direct buffers pinned by their live `in_buf` / `out_buf`
                // local refs; the Java caller is blocked for the whole call,
                // so both stay valid throughout.  The borrowed `input` slice
                // is read in place (no `Vec` copy) and never escapes this
                // synchronous `block_on`.
                let input = unsafe { std::slice::from_raw_parts(in_addr, in_len) };
                let out = unsafe { std::slice::from_raw_parts_mut(out_addr, out_cap) };
                block_on_sync_runtime(vespera_inprocess::dispatch_into_async_borrowed(input, out))
            }));

            let code = match dispatched {
                Ok(vespera_inprocess::DirectWriteResult::Complete(n)) => {
                    // n <= out_cap, and Java buffer capacities are
                    // jint-bounded, so this always fits i32.
                    jint::try_from(n).unwrap_or(DIRECT_UNREPRESENTABLE)
                }
                Ok(vespera_inprocess::DirectWriteResult::Overflow(required)) => {
                    jint::try_from(required).map_or(DIRECT_UNREPRESENTABLE, |r| -r)
                }
                Err(_) => {
                    let err = vespera_inprocess::error_wire(500, "panic in Rust engine");
                    write_response_to_out(out_addr, out_cap, &err)
                }
            };
            Ok(code)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

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
            let input = match read_request_byte_array(env, &request_bytes) {
                Ok(buf) => buf,
                Err(err) => return Ok(env.byte_array_from_slice(&err)?.into()),
            };

            // Promote the OutputStream to Global so we can call
            // .write() from a different attached thread inside
            // the streaming callback.
            let stream_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
            let jvm = env.get_java_vm()?;

            // One per-thread reusable Java chunk buffer for the whole stream.
            let (push_buf, push_buf_lease) =
                checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;

            let header_bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RUNTIME.block_on(vespera_inprocess::dispatch_streaming_async(
                    input,
                    make_push_closure(jvm, stream_global, push_buf),
                ))
            }));
            let header_bytes = header_bytes.map_or_else(
                |_| vespera_inprocess::error_wire(500, "panic in Rust engine"),
                |header_bytes| {
                    mark_streaming_buffer_reusable(push_buf_lease);
                    header_bytes
                },
            );

            Ok(env.byte_array_from_slice(&header_bytes)?.into())
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
            let Ok(header_input) = env.convert_byte_array(&header_bytes) else {
                // A failed conversion (e.g. null array) may leave a pending
                // Java exception; clear it before the follow-up JNI calls.
                clear_pending_exception(env);
                let err = vespera_inprocess::error_wire(
                    400,
                    "invalid header byte array (JNI conversion failed)",
                );
                return Ok(env.byte_array_from_slice(&err)?.into());
            };

            let input_global: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
            // A second InputStream ref for the post-response close — the
            // first is moved into the pull closure (a `Global` is not
            // `Clone`); both are independent GC roots to the same stream.
            let input_for_close: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
            let output_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
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

            let header_response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RUNTIME.block_on(vespera_inprocess::dispatch_bidirectional_streaming_closing(
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
                ))
            }));
            let header_response = header_response.map_or_else(
                |_| vespera_inprocess::error_wire(500, "panic in Rust engine"),
                |header_response| {
                    mark_streaming_buffer_reusable(pull_buf_lease);
                    mark_streaming_buffer_reusable(push_buf_lease);
                    header_response
                },
            );

            Ok(env.byte_array_from_slice(&header_response)?.into())
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
            let header_sent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let header_sent_cb = std::sync::Arc::clone(&header_sent);
            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let header_for_cb = header_global;
                let jvm_for_cb = jvm.clone();
                let push = make_push_closure(jvm, stream_global, push_buf);
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
                            header_sent_cb.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    },
                    push,
                ));
            }));
            if panic_result.is_ok() {
                mark_streaming_buffer_reusable(push_buf_lease);
            } else if !header_sent.load(std::sync::atomic::Ordering::SeqCst)
                && let Ok(fallback) = env.new_global_ref(&header_consumer)
            {
                let err = vespera_inprocess::error_wire(500, "panic in Rust engine");
                let _ = call_header_consumer(env, &fallback, &err);
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
            let Ok(header_input) = env.convert_byte_array(&header_bytes_in) else {
                // A failed conversion (e.g. null array) may leave a pending
                // Java exception; clear it before the follow-up JNI calls.
                clear_pending_exception(env);
                let err = vespera_inprocess::error_wire(
                    400,
                    "invalid header byte array (JNI conversion failed)",
                );
                let _ = call_header_consumer(env, &env.new_global_ref(&header_consumer)?, &err);
                return Ok(());
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
            let header_sent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let header_sent_cb = std::sync::Arc::clone(&header_sent);
            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RUNTIME.block_on(
                    vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                        header_input,
                        make_pull_closure(pull_jvm, pull_global, pull_buf),
                        make_push_closure(push_jvm, push_global, push_buf),
                        |header_bytes: &[u8]| {
                            if with_cached_daemon_env(
                                &header_jvm,
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    call_header_consumer(env, &header_for_cb, header_bytes)
                                },
                            )
                            .is_ok()
                            {
                                header_sent_cb.store(true, std::sync::atomic::Ordering::SeqCst);
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
                );
            }));
            if panic_result.is_ok() {
                mark_streaming_buffer_reusable(pull_buf_lease);
                mark_streaming_buffer_reusable(push_buf_lease);
            } else if !header_sent.load(std::sync::atomic::Ordering::SeqCst)
                && let Ok(fallback) = env.new_global_ref(&header_consumer)
            {
                let err = vespera_inprocess::error_wire(500, "panic in Rust engine");
                let _ = call_header_consumer(env, &fallback, &err);
            }

            Ok(())
        });
    });
}

#[cfg(test)]
#[path = "jni_impl_runtime_config_tests.rs"]
mod runtime_config_tests;

#[cfg(test)]
#[path = "jni_impl_direct_tests.rs"]
mod direct_tests;
