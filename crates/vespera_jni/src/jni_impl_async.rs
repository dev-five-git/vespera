//! Asynchronous `CompletableFuture` dispatch.
//!
//! The `AsyncDispatch.{dispatch0,cancel0}` JNI symbols (behind the public
//! `VesperaBridge.dispatchAsync`) and the exactly-once [`AsyncCompletion`]
//! they share with the poll-first `jni_impl_park.rs`, split out of
//! `jni_impl.rs` to keep that file within the project's 1000-line source
//! cap.  Semantics are unchanged.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::{jboolean, jlong};

use super::{
    RUNTIME_SHUT_DOWN_MSG, async_overloaded_wire, guard_env_void_symbol, panic_wire,
    read_request_byte_array, runtime, runtime_unavailable_wire, try_acquire_async_permit,
    with_cached_daemon_env,
};
use crate::streaming_closures::{CompleteAttempt, complete_future, complete_future_local};

/// The wire completing a dispatch that was not spawned: the runtime-unavailable
/// `503` when there is no runtime (`Ok(false)`), and the panic `500`
/// (`"panicked":"true"`) when scheduling itself panicked, so the host counts
/// it as a panic like every other Rust panic on this path.
fn unscheduled_wire(outcome: &std::thread::Result<bool>) -> Vec<u8> {
    match outcome {
        Ok(_) => runtime_unavailable_wire(),
        Err(_) => panic_wire(),
    }
}

/// `com.devfive.vespera.bridge.AsyncDispatch.dispatch0(CompletableFuture<byte[]>, byte[]) -> long`
/// (behind the public `VesperaBridge.dispatchAsync`)
///
/// **Asynchronous** binary wire-format JNI entry point.  Returns
/// immediately after spawning the dispatch on the shared Tokio
/// runtime.  Completes the supplied `CompletableFuture<byte[]>`
/// from a runtime worker thread once the response is ready.
///
/// Returns the dispatch's **cancellation token** (non-zero) when a task was
/// spawned, or `0` when the future was already completed on the calling
/// thread (overloaded, invalid input, scheduling failure) — nothing to cancel.
///
/// Contract (always-complete):
/// - **success** → `future.complete(responseBytes)`
/// - **JNI conversion failure** → `future.complete(error_wire(400, ...))`
/// - **Rust panic / handler crash** (in the handler or in this symbol's own
///   setup) → `future.complete(panic_wire())` (`500`, `"panicked":"true"`)
///   The future is always completed with a valid wire response —
///   it is never left dangling, even on internal errors.
///
/// The async in-flight permit is released before every completion, also
/// the on-thread ones (overloaded / invalid input / setup failure): a
/// non-async continuation that dispatches again never sees this call's
/// slot still taken.
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
///   variants) also `block_on_shared_runtime(...)` and are *more* damaging to
///   re-enter because they hold a worker across the entire response/request
///   stream.
///
/// Completing the future off the worker (via `spawn_blocking`) was measured at
/// ~16x the per-dispatch cost (`vespera_inprocess` `benches/dispatch.rs`,
/// group `async_completion_ab`: ~1.5 µs inline vs ~24.5 µs hand-off), so the
/// worker-thread completion is kept and this contract is documented instead —
/// matching how Netty / async HTTP clients complete futures from their I/O
/// threads.  Java callers that reach this symbol: the WebFlux proxy
/// (`VesperaReactiveProxyHandler`, every buffered request), `VesperaClient`'s
/// `sendAsync()`, SYNC on a virtual thread with
/// `-Dvespera.virtualThread.pollFirst=false` (`ParkingDispatch`), a servlet
/// proxy whose resolver picks `ASYNC`, and callers doing their own
/// `CompletableFuture` composition (the servlet proxy's default
/// `SmartDispatchModeResolver` never selects `ASYNC`).
///
/// Cancellation: the spawned dispatch is registered with
/// [`vespera_inprocess::cancellable`] under the returned token.  The Java
/// side (`AsyncDispatch`) calls [`Java_com_devfive_vespera_bridge_AsyncDispatch_cancel0`]
/// when the future completes exceptionally (`cancel(..)`, `orTimeout`, an
/// external `completeExceptionally`): the handler future is dropped at its
/// next poll, the task completes the (already cancelled) future with a
/// no-op `503` wire, and the in-flight permit and runtime lease are released
/// with the task.  A cancel that races the normal completion is harmless —
/// the token is deregistered before the future is completed, so a late
/// `cancel0` finds nothing.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_AsyncDispatch_dispatch0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    future_obj: JObject<'local>,
    request_bytes: JByteArray<'local>,
) -> jlong {
    // Cancellation token of the spawned task; stays 0 on every on-thread path.
    let mut token: u64 = 0;
    let future_notified = Arc::new(AtomicBool::new(false));
    let future_notified_body = Arc::clone(&future_notified);
    // The only unrecoverable path is failing to promote the future to a
    // GlobalRef (below): without that ref there is nothing to complete,
    // and a failure there means the JVM is already in trouble.  Every
    // path AFTER the ref exists completes the future, so the
    // always-complete contract holds even on VM-promotion / scheduling
    // failures.
    //
    // JNI-03: the entire body runs under `guard_env_void_symbol` so a panic
    // in the setup that precedes the inner dispatch guard cannot unwind
    // across this `extern "system"` boundary — and is REPORTED, so the
    // fallback below completes the future.
    //
    // Every on-thread completion releases the async in-flight permit FIRST:
    // `complete` runs the future's non-async continuations inline, and one
    // that dispatches again must not find its own slot still taken (the same
    // order as the spawned path and the C ABI's `vespera_dispatch_async`).
    let panicked = guard_env_void_symbol(&mut unowned_env, |env| {
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
        // Admission control BEFORE reading the request: a saturated
        // in-flight cap answers a retryable 503 immediately instead of
        // queueing unbounded tasks (and their request buffers).
        let Some(permit) = try_acquire_async_permit() else {
            let _ = complete_future_local(env, &future_obj, &async_overloaded_wire());
            future_notified_body.store(true, Ordering::Release);
            return Ok(());
        };
        let input = match read_request_byte_array(env, &request_bytes) {
            Ok(buf) => buf,
            Err(err) => {
                drop(permit);
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
                drop(permit);
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
                drop(permit);
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
        // guards the `runtime.handle().spawn` call itself so a scheduling failure completes
        // the future (with a 500) instead of leaving the Java caller
        // hanging; no runtime (shut down) answers the same `503` as every
        // other dispatch mode.
        // The closure captures the permit by value (it moves on into the
        // task), so a failed schedule (no runtime, or a panic) has
        // already released it when the future is completed below.
        let scheduled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(runtime) = runtime() else {
                return false;
            };
            // Completes the future exactly once: with the response, or
            // with a `503` if the runtime drops this task first.
            let completion = AsyncCompletion {
                jvm,
                future: Some(future_for_task),
            };
            // Registered BEFORE the spawn, so a cancel can never miss it.
            let (cancel_token, dispatch) = vespera_inprocess::cancellable(
                std::panic::AssertUnwindSafe(vespera_inprocess::dispatch_from_bytes_async(input))
                    .catch_unwind(),
            );
            runtime.handle().spawn(async move {
                // The in-flight permit is released with the task on every
                // path (including runtime shutdown and cancellation) and,
                // on the normal path, just BEFORE the future is
                // completed — so a caller that observes the completion
                // also observes its slot as free.  The lease is held until
                // the completion so `shutdown_runtime`'s drain waits for it.
                let _lease = runtime;
                let response = match dispatch.await {
                    Some(Ok(response)) => response,
                    Some(Err(_)) => panic_wire(),
                    // Cancelled: the handler was dropped; the Java future
                    // is already done, so this completion is a no-op that
                    // only keeps the always-complete contract.
                    None => vespera_inprocess::error_wire(503, ASYNC_CANCELLED_MSG),
                };
                drop(permit);
                completion.complete(&response);
            });
            token = cancel_token;
            true
        }));
        if !matches!(scheduled, Ok(true)) {
            let wire = unscheduled_wire(&scheduled);
            let _ = complete_future_local(env, &future_obj, &wire);
        }
        future_notified_body.store(true, Ordering::Release);

        Ok(())
    });
    if panicked && !future_notified.load(Ordering::Acquire) && !future_obj.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            complete_future_local(env, &future_obj, &panic_wire())
        });
    }
    // Tokens are sequential from 1, far below `i64::MAX`.
    jlong::try_from(token).unwrap_or(0)
}

/// Body of the `503` wire a cancelled `dispatchAsync` task completes its
/// (already cancelled, so unaffected) future with.
pub const ASYNC_CANCELLED_MSG: &str = "dispatch cancelled by the caller";

/// `com.devfive.vespera.bridge.AsyncDispatch.cancel0(long) -> boolean`
///
/// Cancel the `dispatchAsync` task registered under `token` (returned by
/// [`Java_com_devfive_vespera_bridge_AsyncDispatch_dispatch0`]): its handler
/// future is dropped at the next poll and its in-flight slot is released.
/// `true` when a live dispatch was found; `false` for `0`, a negative or
/// unknown token, or one that already finished — cancelling late is
/// harmless.  Needs no runtime and makes no JNI call back into Java.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_AsyncDispatch_cancel0<'local>(
    _env: EnvUnowned<'local>,
    _class: JClass<'local>,
    token: jlong,
) -> jboolean {
    std::panic::catch_unwind(|| u64::try_from(token).is_ok_and(vespera_inprocess::cancel))
        .unwrap_or(false)
}

/// Completes a `dispatchAsync` `CompletableFuture` exactly once — with the
/// response via [`AsyncCompletion::complete`], or with a `503` wire
/// ([`RUNTIME_SHUT_DOWN_MSG`]) when the task is dropped before that (the
/// runtime shut down with the dispatch still pending), so a Java caller never
/// waits forever.
pub(super) struct AsyncCompletion {
    pub(super) jvm: jni::JavaVM,
    pub(super) future: Option<jni::refs::Global<JObject<'static>>>,
}

impl AsyncCompletion {
    pub(super) fn complete(mut self, response: &[u8]) {
        if let Some(future) = self.future.take() {
            complete_async_future(&self.jvm, &future, response);
        }
    }
}

impl Drop for AsyncCompletion {
    fn drop(&mut self) {
        if let Some(future) = self.future.take() {
            let wire = vespera_inprocess::error_wire(503, RUNTIME_SHUT_DOWN_MSG);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                complete_async_future(&self.jvm, &future, &wire);
            }));
        }
    }
}

/// ALWAYS-COMPLETE CONTRACT: the Java `CompletableFuture` must resolve or
/// `dispatchAsync` callers hang forever, unless the VM refuses this thread's
/// attachment (VM shutdown), which no retry can fix.  The cached-daemon
/// completion can fail BEFORE it reaches Java (an OOM allocating the response
/// `byte[]`, or a daemon attach that the VM still allows); only then is a
/// second, tiny-payload attempt worth making — it is far less likely to OOM,
/// so the future still resolves with an error rather than never.  A
/// `complete()` that WAS invoked (whether it resolved the future or threw)
/// must never be retried: the retry re-enters the same broken Java method and
/// would complete the same future twice.
fn complete_async_future(
    jvm: &jni::JavaVM,
    future: &jni::refs::Global<JObject<'static>>,
    response: &[u8],
) {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_cached_daemon_env(jvm, |env| -> jni::errors::Result<CompleteAttempt> {
            Ok(complete_future(env, future, response))
        })
    }));
    if !matches!(attempt, Ok(Ok(CompleteAttempt::Invoked))) {
        let _ = with_cached_daemon_env(jvm, |env| -> jni::errors::Result<()> {
            let _ = complete_future(
                env,
                future,
                &vespera_inprocess::error_wire(500, "async completion failed"),
            );
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{panic_wire, runtime_unavailable_wire, unscheduled_wire};

    /// A panic while scheduling answers the panic wire (`"panicked":"true"`),
    /// not a plain `500`, so the host classifies it as a panic.
    #[test]
    fn a_panic_while_scheduling_answers_the_panic_wire() {
        let panicked: std::thread::Result<bool> = Err(Box::new("scheduling failed"));
        let wire = unscheduled_wire(&panicked);
        assert_eq!(wire, panic_wire());
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&wire[4..4 + len]).unwrap();
        assert!(header.contains(r#""panicked":"true""#), "{header}");
    }

    /// No runtime (shut down) keeps the runtime-unavailable `503`.
    #[test]
    fn a_missing_runtime_answers_the_unavailable_wire() {
        assert_eq!(unscheduled_wire(&Ok(false)), runtime_unavailable_wire());
    }
}
