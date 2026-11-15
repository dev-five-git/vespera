use std::sync::Arc;

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::jbyteArray;

use crate::daemon_env::with_cached_daemon_env;
use crate::streaming_closures::{close_input_stream, make_pull_closure, make_push_closure};

// Per-thread reusable Java chunk buffers for the streaming paths live in
// a sidecar module to keep this file within the 1000-line source cap.
#[path = "jni_impl_streaming_buffer.rs"]
pub mod streaming_buffer;
use streaming_buffer::{PullPushBuffers, mark_streaming_buffer_reusable, release_pull_lease};

// Runtime / streaming configuration JNI hooks (seeded from
// `VesperaBridge.init()` before the first dispatch) live in a sidecar
// module so this file stays focused on the per-request dispatch symbols.
#[path = "jni_impl_config.rs"]
mod config;
pub use config::{
    DEFAULT_ASYNC_MAX_IN_FLIGHT, async_in_flight, async_max_in_flight, set_async_max_in_flight,
    streaming_chunk_size,
};
use config::{async_overloaded_wire, try_acquire_async_permit};

// Shared runtime lifecycle (lazy build, in-flight leases, deferred hooks,
// graceful shutdown) and its worker / blocking-pool knobs live in
// `vespera_inprocess::runtime`, shared with the C ABI (`vespera_inprocess::ffi`)
// so a cdylib exporting both serves JVM and C hosts from ONE runtime.
pub use vespera_inprocess::runtime::{
    DEFAULT_RUNTIME_MAX_BLOCKING_THREADS, RUNTIME_SHUT_DOWN_MSG, RuntimeHook,
    block_on_shared_runtime, on_shared_runtime, runtime, runtime_max_blocking_threads,
    runtime_unavailable_wire, runtime_worker_threads, set_runtime_max_blocking_threads,
    set_runtime_worker_threads, start_runtime_for_hooks,
};

/// Set by [`mark_jni_loaded`]: a JVM (not a C host) loaded this library.
static JNI_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Mark that the library was loaded by a JVM (`jni_app!` / `jni_apps!` call
/// this at the top of the generated `JNI_OnLoad`); also marks the host as
/// loaded for `vespera::cron`
/// ([`vespera_inprocess::runtime::mark_host_loaded`]).
#[doc(hidden)]
pub fn mark_jni_loaded() {
    JNI_LOADED.store(true, std::sync::atomic::Ordering::SeqCst);
    vespera_inprocess::runtime::mark_host_loaded();
}

/// Whether a JVM loaded this library (see [`mark_jni_loaded`]).
pub fn jni_loaded() -> bool {
    JNI_LOADED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Shut the shared runtime down
/// ([`vespera_inprocess::runtime::shutdown_runtime`]), then drop the stashed
/// DIRECT overflows, which can no longer be redeemed.
pub fn shutdown_runtime(timeout: std::time::Duration) -> bool {
    vespera_inprocess::runtime::shutdown_runtime_then(timeout, direct_stash::clear_stash)
}

/// Check-and-clear the pending Java exception, reporting whether one
/// was pending (and therefore cleared).
///
/// This is the JNI symbol modules' SINGLE check-and-clear
/// implementation: [`clear_pending_exception`] below, the streaming
/// closures in `crate::streaming_closures` and the direct-buffer symbol
/// in `jni_impl_direct.rs` all route through it, so a future policy
/// change (e.g. logging the exception before clearing, or
/// `exception_describe()` in debug builds) needs to touch ONE site
/// instead of drifting across the copies.
///
/// The `bool` exists because every cached-`JMethodID` call site
/// (`call_input_stream_read`, `call_output_stream_write`,
/// `call_consumer_accept`) uses `call_method_unchecked` on the fast
/// path, which (jni 0.22) post-checks and reports a thrown Java exception
/// as `Err(JavaException)` but leaves it PENDING on the thread — as does
/// the checked fallback — so the caller must still clear it before its
/// next JNI call.  The three streaming closures
/// (`make_pull_closure`, `make_push_closure`, `call_header_consumer`)
/// convert that pending exception into an abort with per-caller-specific
/// return values (`RequestChunk::Error` for pull; `JavaException` `Err`
/// for push / header-consumer), so they share the check-clear preamble
/// while keeping their return type distinct.  `#[inline]` folds it back
/// into each caller so codegen matches the previous inline expression.
#[inline]
pub fn take_pending_exception(env: &mut jni::Env<'_>) -> bool {
    if env.exception_check() {
        env.exception_clear();
        true
    } else {
        false
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
///
/// Discard-the-answer wrapper over [`take_pending_exception`] for the
/// majority of call sites that only need the scrub.
#[inline]
pub fn clear_pending_exception(env: &mut jni::Env<'_>) {
    let _ = take_pending_exception(env);
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

/// Hand a wire-format response back to Java as a `byte[]`.
///
/// Every byte-array-returning JNI symbol in this file ends (or bails out)
/// by converting a wire `Vec<u8>` / `&[u8]` into a Java `byte[]` local ref
/// with the identical `Ok(env.byte_array_from_slice(..)?.into())` shape.
/// Naming it once keeps the conversion — and the `?`-propagating failure
/// policy that goes with it — in a single place.
///
/// Deliberately NOT merged with [`byte_array_or_empty`]: that helper carries
/// an extra OOM-of-OOM fallback (scrub the pending exception, hand back an
/// empty `byte[]`) which only the outer panic landing pads want.  Here a
/// failed allocation must keep propagating as `Err` so the enclosing guard
/// applies its own policy.
fn wire_byte_array<'local>(
    env: &mut jni::Env<'local>,
    bytes: &[u8],
) -> jni::errors::Result<JObject<'local>> {
    Ok(env.byte_array_from_slice(bytes)?.into())
}

/// Canonical `500` wire response for a failed streaming setup, ready to
/// return from a streaming symbol's `let ... else` arm.
///
/// `dispatchStreaming` / `dispatchFullStreaming` share the identical
/// recovery shape (scrub the pending exception, hand back a `500` wire
/// response so the Java decoder is never given `null`) INCLUDING the
/// message literal — exactly the literal-drift hazard this file already
/// engineered away for the ingress `400` via [`invalid_input_array_err`].
/// Centralising it here makes a one-sided wording change impossible.
///
/// The message is byte-identical to the prior inlined literals, so the
/// wire `500` body of both streaming symbols is unchanged.
fn streaming_setup_failed_array<'local>(
    env: &mut jni::Env<'local>,
) -> jni::errors::Result<JObject<'local>> {
    clear_pending_exception(env);
    wire_byte_array(env, &streaming_setup_failed_wire())
}

/// The `500` wire every streaming symbol answers when its JNI setup (global
/// refs, the chunk `byte[]`) fails before the dispatch starts — typically
/// heap pressure, never a Rust panic, so it carries no `panicked` flag.
pub fn streaming_setup_failed_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(500, "JNI streaming setup failed")
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
/// Returns whether `body` panicked.  A body that runs through
/// `EnvUnowned::with_env` must use [`guard_env_void_symbol`] instead: that
/// call catches the panic itself, so it never reaches this guard.  Matches
/// the whole-body guard already used by `configureRuntime0` /
/// `configureStreaming0`.
pub fn guard_void_symbol(body: impl FnOnce()) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err()
}

/// Run a **void** JNI symbol's `body` through `EnvUnowned::with_env` under
/// [`guard_void_symbol`] and report whether it panicked — so the caller's
/// panic fallback (complete the `CompletableFuture`, call the header
/// consumer) actually runs.
///
/// `with_env` (jni 0.22) wraps its closure in `catch_unwind` itself and
/// reports a panic as `Outcome::Panic` inside the returned `EnvOutcome`; it
/// never unwinds into an enclosing `catch_unwind`.  Discarding that outcome
/// (`let _ = unowned_env.with_env(..)`) therefore hid every panic of the
/// body and left the fallbacks dead.  An `Err` from `body` is ignored (the
/// body already delivered its own wire / exception).
pub fn guard_env_void_symbol<'local>(
    unowned_env: &mut EnvUnowned<'local>,
    body: impl FnOnce(&mut jni::Env<'local>) -> jni::errors::Result<()>,
) -> bool {
    let mut body_panicked = false;
    let outer_panicked = guard_void_symbol(|| {
        // The panic payload is dropped inside the guard: a payload whose
        // `Drop` panics is caught by it too.
        body_panicked = matches!(
            unowned_env.with_env(body).into_outcome(),
            jni::Outcome::Panic(_)
        );
    });
    outer_panicked || body_panicked
}

/// `500` wire response for a caught panic; its metadata carries
/// `"panicked":"true"` so the host can tell it from a handler `500`.
pub fn panic_wire() -> Vec<u8> {
    vespera_inprocess::panic_error_wire("panic in Rust engine")
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

/// Common outer panic guard for JNI symbols that return `jbyteArray`.
///
/// Runs `body` inside a `catch_unwind`-guarded `with_env` +
/// `resolve::<ThrowRuntimeExAndDefault>` + `into_raw` shape shared by every
/// byte-array-returning JNI dispatch symbol.  A panic in `body` degrades to
/// the standard `byte_array_or_empty(env, &panic_wire())` answer
/// ([`catch_or_panic_byte_array`] inside the `with_env` — `with_env` would
/// otherwise catch it itself and `ThrowRuntimeExAndDefault` would turn it
/// into a thrown `RuntimeException` + `null`); a panic during the JNI env
/// promotion, resolve, or into_raw stage degrades to the same fallback via a
/// second `with_env`, so a panic can never unwind across the
/// `extern "system"` boundary into the JVM.
///
/// The `body` closure receives the promoted `Env` and returns the
/// `jni::errors::Result<JObject<'local>>` to hand back to Java; centralising
/// the resolve + into_raw + panic-fallback shape here keeps the outer-panic
/// policy in exactly one place (same drift-prevention discipline this file
/// already applies to `panic_wire`, `BODY_STREAM_ERROR_MSG`, and
/// `invalid_input_array_err`).
fn guard_byte_array_symbol<'local, F>(unowned_env: &mut EnvUnowned<'local>, body: F) -> jbyteArray
where
    F: FnOnce(&mut jni::Env<'local>) -> jni::errors::Result<JObject<'local>>,
{
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unowned_env
            .with_env(|env| catch_or_panic_byte_array(env, body))
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

/// Common **inner** panic guard of every byte-array-returning symbol (via
/// [`guard_byte_array_symbol`]).
///
/// Wraps `body` in `catch_unwind(AssertUnwindSafe(..))` so a panic inside the
/// setup / read / dispatch stage cannot escape the enclosing `with_env`, and
/// on panic degrades to the standard `byte_array_or_empty(env, &panic_wire())`
/// fallback.
///
/// `body` receives the `Env` as an argument (not a capture) so the borrow
/// checker sees the inner-guard reborrow of `env` end when `catch_unwind`
/// consumes the closure; the outer `env` is then reused for the fallback
/// without a double mutable borrow at the call site.
fn catch_or_panic_byte_array<'local, F>(
    env: &mut jni::Env<'local>,
    body: F,
) -> jni::errors::Result<JObject<'local>>
where
    F: FnOnce(&mut jni::Env<'local>) -> jni::errors::Result<JObject<'local>>,
{
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(env)));
    panic_result.unwrap_or_else(|_| byte_array_or_empty(env, &panic_wire()))
}

/// Inner panic guard of the two **non-header** streaming symbols
/// (`dispatchStreaming`, `dispatchFullStreaming`), which return the response
/// header only after the body went to the Java `OutputStream`.
///
/// `body` flips the `delivered` flag it is handed as soon as a response body
/// byte was handed to Java.  A panic before that is a clean `500` panic wire
/// (nothing was written); a panic after it would put a `500` header behind a
/// partially written body, so it throws the documented
/// [`VesperaStreamAbortedException`](support::AbortReason::Panicked)
/// (`IOException`, `panicked()`) instead — the same "mid-body failure →
/// `IOException`" contract as a body error or a failed write.
fn catch_streaming_byte_array<'local, F>(
    env: &mut jni::Env<'local>,
    body: F,
) -> jni::errors::Result<JObject<'local>>
where
    F: FnOnce(
        &mut jni::Env<'local>,
        &std::cell::Cell<bool>,
    ) -> jni::errors::Result<JObject<'local>>,
{
    let delivered = std::cell::Cell::new(false);
    let panic_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(env, &delivered)));
    match panic_result {
        Ok(response) => response,
        Err(_) if delivered.get() => {
            throw_streaming_abort(env, AbortReason::Panicked);
            Ok(JObject::null())
        }
        Err(_) => byte_array_or_empty(env, &panic_wire()),
    }
}

#[path = "jni_impl_support.rs"]
pub mod support;
use support::{AbortReason, setup_full_stream, setup_stream, throw_streaming_abort, track_pull};

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
    guard_byte_array_symbol(&mut unowned_env, |env| {
        // Read + dispatch under ONE guard: a panic in the ingress read
        // (e.g. allocation failure for an unbounded request) now also
        // degrades to a wire `500` instead of a thrown Java exception.
        let response =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || match read_request_byte_array(env, &request_bytes) {
                    Ok(input) => {
                        block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(input))
                            .unwrap_or_else(runtime_unavailable_wire)
                    }
                    Err(err_wire) => err_wire,
                },
            ))
            .unwrap_or_else(|_| panic_wire());

        wire_byte_array(env, &response)
    })
}

#[path = "jni_impl_direct.rs"]
mod direct;

// Poll-first dispatch behind the bridge's virtual-thread `ParkingDispatch`.
#[path = "jni_impl_park.rs"]
mod park;
pub use direct::stash as direct_stash;

// `AsyncDispatch.{dispatch0,cancel0}` (behind `VesperaBridge.dispatchAsync`)
// and the exactly-once `AsyncCompletion` the poll-first path reuses.
#[path = "jni_impl_async.rs"]
mod async_dispatch;
pub use async_dispatch::ASYNC_CANCELLED_MSG;
use async_dispatch::AsyncCompletion;

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
/// Failure modes: a **pre-streaming** failure (malformed wire, version
/// mismatch, no app registered, a `504` deadline before the header, or a
/// body error before the first chunk) produces a regular `error_wire(...)`
/// header return and the `OutputStream` is **not** written to.  `422`
/// validation errors are hoisted into the returned header like every other
/// mode.  A failure **after** at least one chunk was written (body-stream
/// error, the `OutputStream` throwing, or a Rust panic — then
/// `panicked()`) throws `VesperaStreamAbortedException` (an
/// `IOException`) instead of returning a header: the bytes
/// already written are truncated and the caller must abort its transport.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreaming<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
    output_stream: JObject<'local>,
) -> jbyteArray {
    guard_byte_array_symbol(&mut unowned_env, |env| {
        let response = catch_streaming_byte_array(
            env,
            |env, delivered| -> jni::errors::Result<JObject<'local>> {
                if output_stream.is_null() {
                    return wire_byte_array(
                        env,
                        &vespera_inprocess::error_wire(400, "outputStream must not be null"),
                    );
                }
                let input = match read_request_byte_array(env, &request_bytes) {
                    Ok(buf) => buf,
                    Err(err) => return wire_byte_array(env, &err),
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
                    return streaming_setup_failed_array(env);
                };

                let mut push = make_push_closure(jvm, stream_global, push_buf);
                let dispatched = block_on_shared_runtime(
                    vespera_inprocess::dispatch_streaming_with_outcome_async(
                        input,
                        // This variant emits no flush hints: every call
                        // carries body bytes.
                        |chunk: &[u8]| {
                            delivered.set(true);
                            push(chunk)
                        },
                    ),
                );
                mark_streaming_buffer_reusable(push_buf_lease);
                match dispatched {
                    None => wire_byte_array(env, &runtime_unavailable_wire()),
                    Some((_, outcome)) if outcome.is_truncated() => {
                        throw_streaming_abort(env, AbortReason::BodyAborted);
                        Ok(JObject::null())
                    }
                    Some((header_bytes, _)) => wire_byte_array(env, &header_bytes),
                }
            },
        )?;

        Ok(response)
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
/// wire / unknown version / no app / Rust panic / a failure before the first
/// response chunk produce a normal `error_wire(...)` response in the returned
/// bytes; a failure (incl. a panic) after response bytes were written to
/// `outputStream` throws `VesperaStreamAbortedException` (`IOException`,
/// truncated response).  `outputStream` is only written to, never flushed
/// mid-response (the header comes back only when this call returns).
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
    guard_byte_array_symbol(&mut unowned_env, |env| {
        let response = catch_streaming_byte_array(
            env,
            |env, delivered| -> jni::errors::Result<JObject<'local>> {
                if input_stream.is_null() || output_stream.is_null() {
                    return wire_byte_array(
                        env,
                        &vespera_inprocess::error_wire(
                            400,
                            "inputStream and outputStream must not be null",
                        ),
                    );
                }
                // Read the header byte[] through the shared ingress contract
                // (length cap honoured + pending-exception scrub on failure)
                // rather than a raw `convert_byte_array`, so an oversized header
                // byte[] is rejected before a full Rust-side copy — parity with
                // the buffered dispatch symbols.
                let header_input = match read_request_byte_array(env, &header_bytes) {
                    Ok(buf) => buf,
                    Err(err) => return wire_byte_array(env, &err),
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
                    return streaming_setup_failed_array(env);
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

                // Pull request body chunks from Java InputStream on a tokio
                // blocking thread.  The liveness token tells whether the
                // producer was DETACHED (bounded join) while still parked in
                // `InputStream.read`, in which case the pull array stays in
                // flight and its lease must not be reused.
                let (pull, pull_token) =
                    track_pull(make_pull_closure(pull_jvm, pull_global, pull_buf));
                let mut push = make_push_closure(push_jvm, push_global, push_buf);
                let mut captured_header: Vec<u8> = Vec::new();
                let outcome = block_on_shared_runtime(
                    vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                        header_input,
                        pull,
                        // Push response body chunks to Java OutputStream on
                        // the thread driving the dispatch.  The header-first
                        // dispatcher also sends EMPTY flush hints; this
                        // variant returns its header only afterwards, so a
                        // hint is dropped: flushing the caller's stream (a
                        // servlet response) would commit it before the status
                        // is applied, and a hint is no body byte — a failure
                        // after it is still a clean error response.
                        |chunk: &[u8]| {
                            if chunk.is_empty() {
                                return std::ops::ControlFlow::Continue(());
                            }
                            delivered.set(true);
                            push(chunk)
                        },
                        |h: &[u8]| captured_header.extend_from_slice(h),
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
                release_pull_lease(pull_buf_lease, pull_token.producer_dropped());
                mark_streaming_buffer_reusable(push_buf_lease);
                match outcome {
                    None => wire_byte_array(env, &runtime_unavailable_wire()),
                    Some(vespera_inprocess::StreamOutcome::Complete) => {
                        wire_byte_array(env, &captured_header)
                    }
                    // Failed before any response byte reached Java: a clean
                    // error response is still possible.
                    Some(failed) if !delivered.get() => wire_byte_array(env, &failed.error_wire()),
                    Some(_) => {
                        throw_streaming_abort(env, AbortReason::BodyAborted);
                        Ok(JObject::null())
                    }
                }
            },
        )?;

        Ok(response)
    })
}

#[cfg(test)]
#[path = "jni_impl_runtime_config_tests.rs"]
mod runtime_config_tests;

#[cfg(test)]
#[path = "jni_impl_streaming_abort_tests.rs"]
mod streaming_abort_tests;

#[cfg(test)]
#[path = "jni_impl_shared_runtime_tests.rs"]
mod shared_runtime_tests;

#[cfg(test)]
#[path = "jni_impl_shutdown_tests.rs"]
mod shutdown_tests;
