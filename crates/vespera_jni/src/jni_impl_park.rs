//! Poll-first dispatch for **parked virtual threads**.
//!
//! A virtual thread blocked in a native frame pins its carrier, so the Java
//! bridge (`ParkingDispatch`) never calls the blocking `dispatchBytes` from a
//! virtual thread.  It used to go through `dispatchAsync` + `join()` for every
//! request, paying two cross-thread wake-ups (Java → Tokio worker → Java
//! carrier) even for a handler that finishes without ever waiting — measured
//! at ~10x the latency and ~1/4 the throughput of a pinned call
//! (`libs/vespera-bridge/docs/virtual-thread-park.md`).
//!
//! `AsyncDispatch.dispatchPollFirst0` polls the dispatch future **once** on
//! the calling thread (inside the shared runtime's context, like
//! `dispatchBytes`).  A handler that completes in that poll returns its wire
//! straight away — the carrier was busy only for CPU work, as with any Java
//! code on a virtual thread.  A handler that has to wait (I/O, timers, locks)
//! returns `Pending`: the half-polled future moves to a Tokio worker, which
//! completes the Java `CompletableFuture`, and the virtual thread parks on it
//! — the carrier is never held across a wait.
//!
//! # Task context of the first poll
//!
//! The first poll runs on the Java caller thread with the shared runtime's
//! context entered (`runtime.enter()`, a no-op waker), **outside any Tokio
//! task and outside `block_on`**; only a `Pending` remainder becomes a task.
//! So, in this mode only, one request can observe two task contexts:
//!
//! - `tokio::task::try_id()` is `None` (and `tokio::task::id()` panics) until
//!   the handler's first wait, then returns the spawned task's id; the
//!   `block_on` modes (`dispatchBytes`, `dispatchDirect0`) never have an id,
//!   `dispatchAsync` always has the same one.
//! - The handler may resume on a different OS thread than its first poll, so
//!   `thread_local!` state does not carry across the first `.await`.
//! - The first poll runs without a coop budget, and
//!   `tokio::task::block_in_place` (or a nested `Handle::block_on`) there
//!   just blocks the calling thread — pinning the virtual thread's carrier.
//!
//! `task_local!` values installed with `LocalKey::scope` live in the future
//! itself and behave the same in every mode; `Handle::current()` and
//! `tokio::spawn` work in the first poll.  Handlers that depend on a stable
//! task identity opt out with `-Dvespera.virtualThread.pollFirst=false`
//! (every request runs as one `dispatchAsync` task).  Wrapping the first
//! poll in a task would cost the cross-thread hand-off this mode removes.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::task::{Context, Poll, Waker};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::jbyteArray;

use super::{
    AsyncCompletion, async_overloaded_wire, clear_pending_exception, guard_byte_array_symbol,
    panic_wire, read_request_byte_array, runtime, runtime_unavailable_wire,
    try_acquire_async_permit, wire_byte_array,
};

/// What the first poll left for the Java caller to do.
enum PollFirst {
    /// Finished on the calling thread: hand this wire back.
    Done(Vec<u8>),
    /// Still pending: a Tokio worker completes the `CompletableFuture`.
    Spawned,
}

/// `com.devfive.vespera.bridge.AsyncDispatch.dispatchPollFirst0(CompletableFuture<byte[]>, byte[]) -> byte[]`
///
/// Returns the wire response when the dispatch finished on the calling
/// thread (including every on-thread error: in-flight cap `503`, invalid
/// input `400`/`413`, runtime unavailable, panic `500`) and leaves `future`
/// untouched; returns `null` when the dispatch is still pending and was
/// handed to the shared runtime, which then completes `future` exactly once
/// (always with a wire, `503` if the runtime drops the task).
///
/// The in-flight cap (`vespera.async.maxInFlight`) and the runtime lease
/// cover the whole dispatch, inline or spawned, exactly as `dispatchAsync`.
/// The spawned task is not registered for cancellation: its only caller
/// parks on a future nobody else can cancel.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_AsyncDispatch_dispatchPollFirst0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    future_obj: JObject<'local>,
    request_bytes: JByteArray<'local>,
) -> jbyteArray {
    guard_byte_array_symbol(&mut unowned_env, |env| {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            poll_first(env, &future_obj, &request_bytes)
        }))
        .unwrap_or_else(|_| PollFirst::Done(panic_wire()));
        match outcome {
            PollFirst::Done(wire) => wire_byte_array(env, &wire),
            PollFirst::Spawned => Ok(JObject::null()),
        }
    })
}

fn poll_first(
    env: &mut jni::Env<'_>,
    future_obj: &JObject<'_>,
    request_bytes: &JByteArray<'_>,
) -> PollFirst {
    let Some(permit) = try_acquire_async_permit() else {
        return PollFirst::Done(async_overloaded_wire());
    };
    let input = match read_request_byte_array(env, request_bytes) {
        Ok(input) => input,
        Err(wire) => return PollFirst::Done(wire),
    };
    let Some(runtime) = runtime() else {
        return PollFirst::Done(runtime_unavailable_wire());
    };
    let mut dispatch = Box::pin(
        AssertUnwindSafe(vespera_inprocess::dispatch_from_bytes_async(input)).catch_unwind(),
    );
    // The no-op waker is never relied on: a `Pending` future is spawned, and
    // the task's first poll re-registers every wake-up with the real waker.
    let first = {
        let _context = runtime.enter();
        dispatch
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    };
    match first {
        Poll::Ready(Ok(wire)) => return PollFirst::Done(wire),
        Poll::Ready(Err(_)) => return PollFirst::Done(panic_wire()),
        Poll::Pending => {}
    }

    let promoted = env
        .get_java_vm()
        .and_then(|jvm| env.new_global_ref(future_obj).map(|future| (jvm, future)));
    let Ok((jvm, future)) = promoted else {
        // Cannot complete the Java future from a worker: finish here instead
        // (pinned, but correct) rather than failing the request.
        clear_pending_exception(env);
        let wire = runtime.block_on(dispatch).unwrap_or_else(|_| panic_wire());
        return PollFirst::Done(wire);
    };
    // Completes the future exactly once: with the response, or with a `503`
    // if the runtime drops the task first (also if `spawn` itself panics).
    let completion = AsyncCompletion {
        jvm,
        future: Some(future),
    };
    let handle = runtime.handle();
    handle.spawn(async move {
        // Lease held until the completion so `shutdown_runtime` drains it;
        // the permit is released just before, as in `dispatchAsync`.
        let _lease = runtime;
        let wire = dispatch.await.unwrap_or_else(|_| panic_wire());
        drop(permit);
        completion.complete(&wire);
    });
    PollFirst::Spawned
}
