//! Shared host runtime (feature `runtime`): the one multi-thread Tokio
//! runtime every **foreign-host** dispatch runs on — JNI (`vespera_jni`) and
//! the C ABI ([`crate::ffi`], feature `ffi`) alike.
//!
//! Lazy build, in-flight dispatch leases, deferred runtime hooks (cron) and
//! graceful, irreversible shutdown.  Transport-agnostic: it knows nothing
//! about the JVM or C — the transport crates/modules wrap it.
//!
//! Tuning (first hit wins, then fixed for the process lifetime):
//!
//! | Knob | Setter | Env | Default |
//! |---|---|---|---|
//! | worker threads | [`set_runtime_worker_threads`] | `VESPERA_RUNTIME_WORKERS` | logical CPUs, clamped `1..=1024` |
//! | blocking pool | [`set_runtime_max_blocking_threads`] | `VESPERA_RUNTIME_MAX_BLOCKING_THREADS` | 512, clamped `1..=8192` |

use std::{
    future::Future,
    ops::Deref,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MIN_RUNTIME_WORKERS: usize = 1;
const MAX_RUNTIME_WORKERS: usize = 1024;

/// Default cap on the shared runtime's blocking pool.
///
/// Kept at Tokio's own default (512).  The blocking pool hosts one request
/// producer per in-flight bidirectional upload plus any handler
/// `spawn_blocking` work (e.g. the multipart extractor's temp-file I/O).  A
/// producer waiting for its handler and a handler waiting for a blocking
/// thread can deadlock once the pool is exhausted, so the cap must stay well
/// above the host's request concurrency.  Threads are spawned lazily and
/// reaped after 10 s idle, so a high cap costs nothing when unused.
pub const DEFAULT_RUNTIME_MAX_BLOCKING_THREADS: usize = 512;
const MAX_RUNTIME_MAX_BLOCKING_THREADS: usize = 8192;

static RUNTIME_WORKER_THREADS: OnceLock<Option<usize>> = OnceLock::new();
static RUNTIME_MAX_BLOCKING_THREADS: OnceLock<usize> = OnceLock::new();

/// Parse a `VESPERA_RUNTIME_WORKERS` value: absent → `None` (Tokio's
/// default); present but unparseable → one-time stderr warning + `None`;
/// valid → clamped to `[1, 1024]`.
fn parse_worker_threads(raw: Option<&str>) -> Option<usize> {
    let raw = raw?;
    raw.trim().parse::<usize>().map_or_else(
        |_| {
            eprintln!(
                "vespera: ignoring invalid VESPERA_RUNTIME_WORKERS={raw:?} \
                 (expected a positive integer); using the Tokio default \
                 (logical CPU count)"
            );
            None
        },
        |v| Some(v.clamp(MIN_RUNTIME_WORKERS, MAX_RUNTIME_WORKERS)),
    )
}

/// Worker thread count for the shared runtime, resolved once:
/// [`set_runtime_worker_threads`] (called before the runtime is first
/// used), then `VESPERA_RUNTIME_WORKERS`, then `None` — Tokio's default
/// (number of logical CPUs).  Values are clamped to `[1, 1024]`.
#[must_use]
pub fn runtime_worker_threads() -> Option<usize> {
    *RUNTIME_WORKER_THREADS.get_or_init(|| {
        parse_worker_threads(std::env::var("VESPERA_RUNTIME_WORKERS").ok().as_deref())
    })
}

/// Override the shared runtime's worker thread count **before the first
/// dispatch**.  Returns `false` when the value was already fixed.  Clamped
/// to `[1, 1024]`.
pub fn set_runtime_worker_threads(workers: usize) -> bool {
    RUNTIME_WORKER_THREADS
        .set(Some(
            workers.clamp(MIN_RUNTIME_WORKERS, MAX_RUNTIME_WORKERS),
        ))
        .is_ok()
}

/// Blocking-pool cap of the shared runtime.  Precedence:
/// [`set_runtime_max_blocking_threads`], then
/// `VESPERA_RUNTIME_MAX_BLOCKING_THREADS`, then
/// [`DEFAULT_RUNTIME_MAX_BLOCKING_THREADS`] (512).  Clamped to `[1, 8192]`.
#[must_use]
pub fn runtime_max_blocking_threads() -> usize {
    *RUNTIME_MAX_BLOCKING_THREADS.get_or_init(|| {
        crate::config::read_env_clamped(
            "VESPERA_RUNTIME_MAX_BLOCKING_THREADS",
            DEFAULT_RUNTIME_MAX_BLOCKING_THREADS,
            1,
            MAX_RUNTIME_MAX_BLOCKING_THREADS,
        )
    })
}

/// Override the shared runtime's blocking-pool cap **before the first
/// dispatch**.  Returns `false` when already fixed.  Clamped to `[1, 8192]`.
pub fn set_runtime_max_blocking_threads(threads: usize) -> bool {
    RUNTIME_MAX_BLOCKING_THREADS
        .set(threads.clamp(1, MAX_RUNTIME_MAX_BLOCKING_THREADS))
        .is_ok()
}

/// Multi-threaded Tokio runtime shared by ALL host dispatches — sync,
/// direct, async and streaming alike.
///
/// Dispatch only ever uses the cloned [`Handle`](tokio::runtime::Handle);
/// the owning [`Runtime`](tokio::runtime::Runtime) sits behind a `Mutex` so
/// [`shutdown_runtime`] can take it out and shut it down gracefully.  The
/// hot path never touches the mutex.
static RUNTIME: OnceLock<Option<SharedRuntime>> = OnceLock::new();

/// Set once by [`shutdown_runtime`]; from then on [`runtime`] returns `None`
/// and [`runtime_unavailable_wire`] answers `503`.
static RUNTIME_SHUT_DOWN: AtomicBool = AtomicBool::new(false);

/// Number of live `RuntimeLease`s (dispatches using the runtime).
/// Incremented BEFORE the shut-down flag is checked, and the flag is set
/// before [`shutdown_runtime`] reads this counter (both `SeqCst`), so a
/// dispatch either sees the flag or is counted by the drain — never neither.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

const NO_SHUTDOWN_THREAD_MSG: &str =
    "vespera: could not start the shutdown thread; runtime left running";

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

/// Whether [`shutdown_runtime`] has been called.
#[must_use]
pub fn runtime_shut_down() -> bool {
    RUNTIME_SHUT_DOWN.load(Ordering::Acquire)
}

/// Wire response for a dispatch that found no runtime: `503` after
/// [`shutdown_runtime`], `500` when the runtime could not be built.
#[must_use]
pub fn runtime_unavailable_wire() -> Vec<u8> {
    let shut_down = runtime_shut_down();
    crate::error_wire(
        if shut_down { 503 } else { 500 },
        if shut_down {
            RUNTIME_SHUT_DOWN_MSG
        } else {
            "failed to create Tokio runtime"
        },
    )
}

/// Drive a synchronous host dispatch to completion on the shared runtime
/// from the calling (host) thread, holding a [`RuntimeLease`] for the
/// duration.  `None` when the runtime is unavailable.
///
/// The request future itself is polled on the calling thread inside
/// `block_on`, but everything it spawns lives on the shared runtime: a
/// detached `tokio::spawn` keeps making progress on the worker threads after
/// the call returns, and runtime-bound resources (timers, I/O registrations,
/// pooled connections) stay valid for later dispatches from ANY host thread.
///
/// Must not be called from inside the runtime (a Tokio worker thread): the
/// resulting `block_on` panic must be caught by the caller's FFI guard.
pub fn block_on_shared_runtime<F>(future: F) -> Option<F::Output>
where
    F: Future,
{
    runtime().map(|runtime| runtime.block_on(future))
}

/// Shut the shared runtime down: refuse new dispatches, wait up to
/// `timeout` for in-flight ones (every [`RuntimeLease`]), then shut the
/// runtime down with what is left of `timeout`.  Background tasks (cron
/// jobs, detached `tokio::spawn`s) stop with the runtime.
///
/// Irreversible for the process: every later dispatch answers `503`
/// ([`RUNTIME_SHUT_DOWN_MSG`]); a runtime that was never started is simply
/// never built.  Returns `false` when the runtime had already been shut
/// down.  Called from inside a Tokio runtime, dispatches are refused
/// immediately and the drain + shutdown continue on a background thread —
/// waiting there would block the very worker the drain needs.
pub fn shutdown_runtime(timeout: Duration) -> bool {
    shutdown_runtime_then(timeout, || {})
}

/// [`shutdown_runtime`], running `cleanup` once the runtime is down (on the
/// background thread when called inside a runtime).  Transports use it to
/// drop state that only makes sense while the runtime lives (the JNI DIRECT
/// overflow stash).
pub fn shutdown_runtime_then(timeout: Duration, cleanup: fn()) -> bool {
    if RUNTIME_SHUT_DOWN.swap(true, Ordering::SeqCst) {
        return false;
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        // Dispatches are refused already; if no thread can be spawned, leave the
        // runtime to the process exit rather than blocking a worker.
        let spawned = std::thread::Builder::new()
            .name("vespera-shutdown".to_owned())
            .spawn(move || finish_shutdown(timeout, cleanup));
        let _ = spawned.map_err(|_| eprintln!("{NO_SHUTDOWN_THREAD_MSG}"));
        return true;
    }
    finish_shutdown(timeout, cleanup);
    true
}

/// Drain in-flight leases (bounded by `timeout`), shut the runtime down,
/// then run `cleanup`.
fn finish_shutdown(timeout: Duration, cleanup: fn()) {
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
    cleanup();
    drop(HOOKS.lock().map(|mut hooks| std::mem::take(&mut *hooks)));
}

/// Wait until no `RuntimeLease` is alive or `deadline` passes.
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
static HOST_LOADED: AtomicBool = AtomicBool::new(false);

/// Run `hook` with the shared runtime's handle once it exists: immediately
/// when it is already built, otherwise when [`start_runtime_for_hooks`] or
/// the first dispatch builds it.  Dropped after [`shutdown_runtime`].  Used
/// by `vespera::cron` so jobs registered from a host's load hook
/// (`JNI_OnLoad`, `vespera_init`) run on the configured shared runtime and
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
    // A racing flush that already took the hooks finds the flag cleared.
    let pending = HOOKS_PENDING.swap(false, Ordering::SeqCst);
    let hooks = if pending {
        HOOKS
            .lock()
            .map(|mut hooks| std::mem::take(&mut *hooks))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    for hook in hooks {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(handle))).is_err() {
            eprintln!("vespera: a shared-runtime hook panicked");
        }
    }
}

/// Mark that a foreign host loaded this library (`JNI_OnLoad` from
/// `jni_app!` / `jni_apps!`, `vespera_init` from `ffi_app!` / `ffi_apps!`).
/// From then on `vespera::cron` hands jobs to [`on_shared_runtime`] instead
/// of starting its own scheduler thread.
pub fn mark_host_loaded() {
    HOST_LOADED.store(true, Ordering::SeqCst);
}

/// Whether a foreign host loaded this library (see [`mark_host_loaded`]).
#[must_use]
pub fn host_loaded() -> bool {
    HOST_LOADED.load(Ordering::SeqCst)
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
        assert!(drain_in_flight(Instant::now()));
    }

    #[test]
    fn worker_thread_env_parsing() {
        assert_eq!(parse_worker_threads(None), None);
        assert_eq!(parse_worker_threads(Some("nope")), None);
        assert_eq!(parse_worker_threads(Some(" 0 ")), Some(1));
        assert_eq!(parse_worker_threads(Some("4")), Some(4));
        assert_eq!(parse_worker_threads(Some("99999")), Some(1024));
    }
}
