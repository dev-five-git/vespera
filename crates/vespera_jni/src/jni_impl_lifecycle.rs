//! Shared Tokio runtime lifecycle: lazy build, in-flight dispatch leases,
//! deferred runtime hooks (cron) and graceful shutdown.

use std::{
    future::Future,
    ops::Deref,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use super::{runtime_max_blocking_threads, runtime_worker_threads};

/// Multi-threaded Tokio runtime shared across ALL JNI calls — sync
/// (`dispatchBytes`, `dispatchDirect0`), async and streaming alike.
///
/// Worker thread count defaults to Tokio's heuristic (number of
/// logical CPUs) and can be capped for embeddings where the JVM's
/// own thread pools (e.g. Tomcat) compete for the same cores —
/// see [`runtime_worker_threads`].  The blocking pool is capped by
/// [`runtime_max_blocking_threads`] (default
/// [`super::DEFAULT_RUNTIME_MAX_BLOCKING_THREADS`]), which bounds the whole
/// process: there is no per-Java-thread runtime.
///
/// Dispatch only ever uses the cloned [`Handle`](tokio::runtime::Handle);
/// the owning [`Runtime`](tokio::runtime::Runtime) sits behind a `Mutex` so
/// [`shutdown_runtime`] (the `shutdown0` JNI hook) can take it out and shut
/// it down gracefully.  The hot path never touches the mutex.
static RUNTIME: OnceLock<Option<SharedRuntime>> = OnceLock::new();

/// Set once by [`shutdown_runtime`]; from then on [`runtime`] returns `None`
/// and [`runtime_unavailable_wire`] answers `503`.
static RUNTIME_SHUT_DOWN: AtomicBool = AtomicBool::new(false);

/// Number of live [`RuntimeLease`]s (dispatches using the runtime).
/// Incremented BEFORE the shut-down flag is checked, and the flag is set
/// before [`shutdown_runtime`] reads this counter (both `SeqCst`), so a
/// dispatch either sees the flag or is counted by the drain — never neither.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Wire `503` body once the shared runtime has been shut down.
pub const RUNTIME_SHUT_DOWN_MSG: &str = "vespera runtime has been shut down";

struct SharedRuntime {
    handle: tokio::runtime::Handle,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
}

fn build_shared_runtime() -> Option<SharedRuntime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(workers) = runtime_worker_threads() {
        builder.worker_threads(workers);
    }
    let runtime = builder
        .max_blocking_threads(runtime_max_blocking_threads())
        .enable_all()
        .build()
        .ok()?;
    Some(SharedRuntime {
        handle: runtime.handle().clone(),
        runtime: Mutex::new(Some(runtime)),
    })
}

/// A dispatch's claim on the shared runtime: derefs to its
/// [`Handle`](tokio::runtime::Handle) and keeps [`shutdown_runtime`]'s drain
/// waiting until dropped.  Move it into a spawned task to cover the task.
#[must_use = "the lease marks the dispatch in flight only while it is alive"]
pub struct RuntimeLease {
    handle: &'static tokio::runtime::Handle,
}

impl RuntimeLease {
    /// The `'static` handle (outlives the lease — for spawning a task that
    /// takes the lease with it).
    #[must_use]
    pub const fn handle(&self) -> &'static tokio::runtime::Handle {
        self.handle
    }
}

impl Deref for RuntimeLease {
    type Target = tokio::runtime::Handle;

    fn deref(&self) -> &tokio::runtime::Handle {
        self.handle
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Decrements [`IN_FLIGHT`] for a claim that did not turn into a lease.
struct Claim;

impl Drop for Claim {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Lease on the shared runtime (built on first use), or `None` when it
/// could not be built or has been shut down (see
/// [`runtime_unavailable_wire`] for the matching wire response).
pub fn runtime() -> Option<RuntimeLease> {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let claim = Claim;
    if RUNTIME_SHUT_DOWN.load(Ordering::SeqCst) {
        return None;
    }
    let handle = &RUNTIME.get_or_init(build_shared_runtime).as_ref()?.handle;
    if HOOKS_PENDING.load(Ordering::Relaxed) {
        flush_hooks(handle);
    }
    std::mem::forget(claim);
    Some(RuntimeLease { handle })
}

/// Wire response for a dispatch that found no runtime: `503` after
/// [`shutdown_runtime`], `500` when the runtime could not be built.
pub fn runtime_unavailable_wire() -> Vec<u8> {
    if RUNTIME_SHUT_DOWN.load(Ordering::Acquire) {
        vespera_inprocess::error_wire(503, RUNTIME_SHUT_DOWN_MSG)
    } else {
        vespera_inprocess::error_wire(500, "failed to create Tokio runtime")
    }
}

/// Drive a synchronous JNI dispatch to completion on the shared
/// multi-thread [`RUNTIME`] from the calling (Java) thread, holding a
/// [`RuntimeLease`] for the duration.
///
/// The request future itself is polled on the calling thread inside
/// `block_on`, but everything it spawns lives on the shared runtime: a
/// detached `tokio::spawn` keeps making progress on the worker threads after
/// the call returns, and runtime-bound resources (timers, I/O registrations,
/// pooled connections) created during one dispatch stay valid for later
/// dispatches from ANY Java thread.
///
/// Must not be called from inside the runtime (e.g. an inline
/// `CompletableFuture` continuation on a worker — see `dispatchAsync`); the
/// resulting `block_on` panic is caught by the symbol guards and degrades to a
/// `500`.
pub fn block_on_shared_runtime<F>(future: F) -> Option<F::Output>
where
    F: Future,
{
    runtime().map(|runtime| runtime.block_on(future))
}

/// Shut the shared runtime down: refuse new dispatches, wait up to
/// `timeout` for in-flight ones (every [`RuntimeLease`], including
/// `dispatchAsync` tasks, which complete their `CompletableFuture`), then
/// shut the runtime down with what is left of `timeout`.  Tasks still
/// running after that are dropped; a dropped `dispatchAsync` task completes
/// its future with a `503` wire.  Background tasks (cron jobs, detached
/// `tokio::spawn`s) stop with the runtime.
///
/// Irreversible for the process: every later dispatch answers `503`
/// ([`RUNTIME_SHUT_DOWN_MSG`]); a runtime that was never started is simply
/// never built.  Returns `false` when the runtime had already been shut
/// down.  Called from inside a Tokio runtime (e.g. a `CompletableFuture`
/// continuation running on a worker), dispatches are refused immediately and
/// the drain + shutdown continue on a background thread — waiting there
/// would block the very worker the drain needs.
pub fn shutdown_runtime(timeout: Duration) -> bool {
    if RUNTIME_SHUT_DOWN.swap(true, Ordering::SeqCst) {
        return false;
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        let spawned = std::thread::Builder::new()
            .name("vespera-shutdown".to_owned())
            .spawn(move || finish_shutdown(timeout));
        if spawned.is_err() {
            // No thread: refuse dispatches (already done) and leave the
            // runtime to the process exit rather than blocking a worker.
            eprintln!("vespera: could not start the shutdown thread; runtime left running");
        }
        return true;
    }
    finish_shutdown(timeout);
    true
}

/// Drain in-flight leases (bounded by `timeout`), then shut the runtime down.
fn finish_shutdown(timeout: Duration) {
    // A never-started runtime stays unbuilt (a concurrent first dispatch
    // that is mid-build completes first and is shut down below).
    let _ = RUNTIME.set(None);
    let deadline = Instant::now() + timeout;
    drain_in_flight(deadline);
    let owned = RUNTIME
        .get()
        .and_then(Option::as_ref)
        .and_then(|shared| shared.runtime.lock().ok()?.take());
    if let Some(runtime) = owned {
        runtime.shutdown_timeout(deadline.saturating_duration_since(Instant::now()));
    }
    // Stashed DIRECT overflows can no longer be redeemed.
    super::direct_stash::clear_stash();
    drop(HOOKS.lock().map(|mut hooks| std::mem::take(&mut *hooks)));
}

/// Wait until no [`RuntimeLease`] is alive or `deadline` passes.
fn drain_in_flight(deadline: Instant) -> bool {
    loop {
        if IN_FLIGHT.load(Ordering::SeqCst) == 0 {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(2)));
    }
}

/// Work deferred until the shared runtime exists (see [`on_shared_runtime`]).
pub type RuntimeHook = Box<dyn FnOnce(&tokio::runtime::Handle) + Send>;

static HOOKS: Mutex<Vec<RuntimeHook>> = Mutex::new(Vec::new());
static HOOKS_PENDING: AtomicBool = AtomicBool::new(false);
static JNI_LOADED: AtomicBool = AtomicBool::new(false);

/// Run `hook` with the shared runtime's handle once it exists: immediately
/// when it is already built, otherwise when [`start_runtime_for_hooks`]
/// (`VesperaNativeConfig.startRuntime0`, called by `VesperaBridge.init()`
/// right after the runtime knobs are applied) or the first dispatch builds
/// it.  Dropped after [`shutdown_runtime`].  Used by `vespera::cron` so jobs
/// registered from `JNI_OnLoad` run on the configured shared runtime and
/// stop with it.
pub fn on_shared_runtime(hook: RuntimeHook) {
    if RUNTIME_SHUT_DOWN.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(mut hooks) = HOOKS.lock() {
        hooks.push(hook);
    }
    HOOKS_PENDING.store(true, Ordering::SeqCst);
    if let Some(Some(shared)) = RUNTIME.get() {
        flush_hooks(&shared.handle);
    }
}

/// Build the runtime now when hooks wait for it.  Returns whether it runs.
pub fn start_runtime_for_hooks() -> bool {
    HOOKS_PENDING.load(Ordering::SeqCst) && runtime().is_some()
}

fn flush_hooks(handle: &tokio::runtime::Handle) {
    if !HOOKS_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    let hooks = HOOKS
        .lock()
        .map(|mut hooks| std::mem::take(&mut *hooks))
        .unwrap_or_default();
    for hook in hooks {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(handle))).is_err() {
            eprintln!("vespera: a shared-runtime hook panicked");
        }
    }
}

/// Mark that the library was loaded by a JVM (`jni_app!` / `jni_apps!` call
/// this at the top of the generated `JNI_OnLoad`).
#[doc(hidden)]
pub fn mark_jni_loaded() {
    JNI_LOADED.store(true, Ordering::SeqCst);
}

/// Whether a JVM loaded this library (see [`mark_jni_loaded`]).
pub fn jni_loaded() -> bool {
    JNI_LOADED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_reports_timeout_while_a_claim_is_alive() {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        let claim = Claim;
        assert!(!drain_in_flight(Instant::now() + Duration::from_millis(5)));
        drop(claim);
    }
}
