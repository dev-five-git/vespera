//! Runtime / streaming / limit configuration JNI hooks.
//!
//! These symbols are seeded from `VesperaBridge.init()` **before the first
//! dispatch** and then fixed for the process lifetime.  They are split out of
//! `jni_impl.rs` (which owns the per-request dispatch symbols) so each file
//! keeps a single concern — and stays within the 1000-line source cap.
//!
//! Every `configure*0` hook returns a `jboolean`: `JNI_TRUE` when every value
//! it was asked to apply was applied, `JNI_FALSE` when at least one knob was
//! already fixed (a previous call, an earlier dispatch read it, or — for the
//! runtime knobs — the shared runtime already started).  The Java side
//! surfaces `false` instead of silently dropping the configuration.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jni::EnvUnowned;
use jni::objects::JClass;
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong};

use super::guard_void_symbol;

const MIN_RUNTIME_WORKERS: usize = 1;
const MAX_RUNTIME_WORKERS: usize = 1024;

/// Default cap on the shared runtime's blocking pool.
///
/// Kept at Tokio's own default (512).  The blocking pool hosts one request
/// producer per in-flight bidirectional upload (parked in
/// `InputStream.read`) plus any handler `spawn_blocking` work (e.g. the
/// multipart extractor's temp-file I/O).  A producer waiting for its handler
/// and a handler waiting for a blocking thread can deadlock once the pool is
/// exhausted, so the cap must stay well above the host's request concurrency
/// (Tomcat's default pool is 200 threads).  Threads are spawned lazily and
/// reaped after 10 s idle, so a high cap costs nothing when unused; it now
/// bounds the whole process (sync / direct dispatch no longer build one
/// runtime — and one blocking pool — per Java thread).
pub const DEFAULT_RUNTIME_MAX_BLOCKING_THREADS: usize = 512;
const MAX_RUNTIME_MAX_BLOCKING_THREADS: usize = 8192;

/// Default cap on concurrently in-flight `dispatchAsync` calls.
///
/// `dispatchAsync` returns immediately, so a caller that fires it in a loop
/// without its own backpressure would otherwise queue unbounded tasks (and
/// their request buffers) on the runtime.  4096 is far above realistic
/// in-process concurrency for a JVM host (servlet pools are typically a few
/// hundred threads) yet bounds a runaway producer; excess calls complete
/// immediately with a retryable `503` wire response.  `0` disables the cap.
pub const DEFAULT_ASYNC_MAX_IN_FLIGHT: usize = 4096;
const MAX_ASYNC_MAX_IN_FLIGHT: usize = 1 << 24;

static RUNTIME_WORKER_THREADS: OnceLock<Option<usize>> = OnceLock::new();
static RUNTIME_MAX_BLOCKING_THREADS: OnceLock<usize> = OnceLock::new();
static ASYNC_MAX_IN_FLIGHT: OnceLock<usize> = OnceLock::new();
static ASYNC_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Read a `usize` env knob: absent → `default`; present but unparseable →
/// one-time stderr warning + `default` (callers resolve through a
/// `OnceLock`, so the warning fires at most once); valid → clamped.
fn read_env_usize(var: &str, default: usize, min: usize, max: usize) -> usize {
    let Ok(raw) = std::env::var(var) else {
        return default;
    };
    raw.trim().parse::<usize>().map_or_else(
        |_| {
            eprintln!(
                "vespera: ignoring invalid {var}={raw:?} (expected a non-negative \
                 integer); using the default {default}"
            );
            default
        },
        |v| v.clamp(min, max),
    )
}

/// Worker thread count for the shared Tokio runtime, resolved once
/// (first hit wins, then fixed for the process lifetime):
///
/// 1. [`set_runtime_worker_threads`] called before the runtime is
///    first used (the `configureRuntime0` JNI hook from
///    `VesperaBridge.init()` lands here)
/// 2. `VESPERA_RUNTIME_WORKERS` environment variable
/// 3. `None` — Tokio's default (number of logical CPUs)
///
/// Values are clamped to `[1, 1024]`.  An env value that is present but
/// fails to parse emits a one-time stderr warning and falls back to the
/// Tokio default.
#[must_use]
pub fn runtime_worker_threads() -> Option<usize> {
    *RUNTIME_WORKER_THREADS.get_or_init(|| {
        // Absent env var: silently fall through to the Tokio default — only a
        // PRESENT-but-INVALID value triggers the warning.
        let raw = std::env::var("VESPERA_RUNTIME_WORKERS").ok()?;
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

/// Blocking-pool cap of the shared runtime.  Precedence:
/// [`set_runtime_max_blocking_threads`], then
/// `VESPERA_RUNTIME_MAX_BLOCKING_THREADS`, then
/// [`DEFAULT_RUNTIME_MAX_BLOCKING_THREADS`] (512).  Clamped to `[1, 8192]`.
#[must_use]
pub fn runtime_max_blocking_threads() -> usize {
    *RUNTIME_MAX_BLOCKING_THREADS.get_or_init(|| {
        read_env_usize(
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

/// Cap on concurrently in-flight `dispatchAsync` calls.  Precedence:
/// [`set_async_max_in_flight`], then `VESPERA_ASYNC_MAX_IN_FLIGHT`, then
/// [`DEFAULT_ASYNC_MAX_IN_FLIGHT`] (4096).  `0` = unlimited.
#[must_use]
pub fn async_max_in_flight() -> usize {
    *ASYNC_MAX_IN_FLIGHT.get_or_init(|| {
        read_env_usize(
            "VESPERA_ASYNC_MAX_IN_FLIGHT",
            DEFAULT_ASYNC_MAX_IN_FLIGHT,
            0,
            MAX_ASYNC_MAX_IN_FLIGHT,
        )
    })
}

/// Override the `dispatchAsync` in-flight cap **before the first async
/// dispatch**.  `0` = unlimited.  Returns `false` when already fixed.
pub fn set_async_max_in_flight(max: usize) -> bool {
    ASYNC_MAX_IN_FLIGHT
        .set(max.min(MAX_ASYNC_MAX_IN_FLIGHT))
        .is_ok()
}

/// One admitted `dispatchAsync` slot; releases it on drop (task completion,
/// a caught panic, or the runtime dropping the task at shutdown).
pub struct AsyncPermit {
    /// The counter this permit holds a slot in; `None` when uncapped.
    counter: Option<&'static AtomicUsize>,
}

impl Drop for AsyncPermit {
    fn drop(&mut self) {
        if let Some(counter) = self.counter {
            counter.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Admit one `dispatchAsync` call, or `None` when the in-flight cap is
/// reached (the caller completes the future with a `503` immediately).
#[must_use]
pub fn try_acquire_async_permit() -> Option<AsyncPermit> {
    admit(async_max_in_flight(), &ASYNC_IN_FLIGHT)
}

fn admit(max: usize, in_flight: &'static AtomicUsize) -> Option<AsyncPermit> {
    if max == 0 {
        return Some(AsyncPermit { counter: None });
    }
    let previous = in_flight.fetch_add(1, Ordering::AcqRel);
    if previous >= max {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    Some(AsyncPermit {
        counter: Some(in_flight),
    })
}

/// `503` wire response for an async dispatch rejected by the in-flight cap.
#[must_use]
pub fn async_overloaded_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(
        503,
        "too many in-flight async dispatches \
         (vespera.async.maxInFlight / VESPERA_ASYNC_MAX_IN_FLIGHT)",
    )
}

/// Per-chunk buffer size for streaming dispatches.
///
/// Resolved once per process by
/// [`vespera_inprocess::streaming_chunk_bytes`] (default 256 KiB;
/// override via the `VESPERA_STREAMING_CHUNK_BYTES` env var or the
/// `configureStreaming0` JNI setter called from
/// `VesperaBridge.init()`).  Subsequent calls are a single atomic load.
pub fn streaming_chunk_size() -> usize {
    vespera_inprocess::streaming_chunk_bytes()
}

const fn to_jboolean(applied: bool) -> jboolean {
    if applied { JNI_TRUE } else { JNI_FALSE }
}

/// Shared body of the `configure*0` hooks: run `apply` under the void-symbol
/// panic guard and map the outcome to a `jboolean` (`false` on a caught
/// panic as well — nothing was provably applied).
fn guarded_configure(apply: impl FnOnce() -> bool) -> jboolean {
    let mut applied = false;
    let panicked = guard_void_symbol(|| applied = apply());
    to_jboolean(applied && !panicked)
}

/// Apply an optional `usize` knob: a negative JNI value means "not
/// configured" (nothing to apply → `true`); otherwise run `set`.
fn apply_optional(value: jlong, set: impl FnOnce(usize) -> bool) -> bool {
    usize::try_from(value).map_or(true, set)
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureRuntime0(int) -> boolean`
///
/// Seeds the shared Tokio runtime's worker thread count **before the
/// first dispatch**.  Values `<= 0` leave the setting untouched (env var /
/// Tokio default applies) and report `true`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureRuntime0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    worker_threads: jint,
) -> jboolean {
    guarded_configure(|| match usize::try_from(worker_threads) {
        Ok(workers) if workers > 0 => set_runtime_worker_threads(workers),
        _ => true,
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureMaxBlockingThreads0(int) -> boolean`
///
/// Seeds the shared runtime's blocking-pool cap before the first dispatch.
/// Values `<= 0` leave it untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureMaxBlockingThreads0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_blocking_threads: jint,
) -> jboolean {
    guarded_configure(|| match usize::try_from(max_blocking_threads) {
        Ok(threads) if threads > 0 => set_runtime_max_blocking_threads(threads),
        _ => true,
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureStreaming0(int, int) -> boolean`
///
/// Seeds the process-wide streaming configuration **before the first
/// dispatch**.  Values `<= 0` leave the corresponding setting untouched.
/// Returns `true` only when every provided value was applied.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureStreaming0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    chunk_bytes: jint,
    channel_capacity: jint,
) -> jboolean {
    guarded_configure(|| {
        // Both setters run regardless of the first one's outcome.
        let chunk_ok = match usize::try_from(chunk_bytes) {
            Ok(bytes) if bytes > 0 => vespera_inprocess::set_streaming_chunk_bytes(bytes),
            _ => true,
        };
        let capacity_ok = match usize::try_from(channel_capacity) {
            Ok(slots) if slots > 0 => vespera_inprocess::set_streaming_channel_capacity(slots),
            _ => true,
        };
        chunk_ok && capacity_ok
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureMaxRequestBytes0(long) -> boolean`
///
/// Buffered-path ingress cap ([`vespera_inprocess::set_max_request_bytes`]);
/// `0` = unlimited, negative = untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureMaxRequestBytes0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_bytes: jlong,
) -> jboolean {
    guarded_configure(|| apply_optional(max_bytes, vespera_inprocess::set_max_request_bytes))
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureMaxStreamingRequestBytes0(long) -> boolean`
///
/// Bidirectional-streaming request-body cap
/// ([`vespera_inprocess::set_max_streaming_request_bytes`]); `0` =
/// unlimited, negative = untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureMaxStreamingRequestBytes0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_bytes: jlong,
) -> jboolean {
    guarded_configure(|| {
        apply_optional(
            max_bytes,
            vespera_inprocess::set_max_streaming_request_bytes,
        )
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureMaxBufferedResponseBytes0(long) -> boolean`
///
/// Buffered-response cap
/// ([`vespera_inprocess::set_max_buffered_response_bytes`]); `0` =
/// unlimited, negative = untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureMaxBufferedResponseBytes0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_bytes: jlong,
) -> jboolean {
    guarded_configure(|| {
        apply_optional(
            max_bytes,
            vespera_inprocess::set_max_buffered_response_bytes,
        )
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureDispatchTimeout0(long) -> boolean`
///
/// Per-dispatch deadline in milliseconds
/// ([`vespera_inprocess::set_dispatch_timeout`]); `0` = disabled, negative =
/// untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureDispatchTimeout0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    timeout_millis: jlong,
) -> jboolean {
    guarded_configure(|| {
        apply_optional(timeout_millis, |ms| {
            vespera_inprocess::set_dispatch_timeout(Duration::from_millis(
                u64::try_from(ms).unwrap_or(u64::MAX),
            ))
        })
    })
}

/// `com.devfive.vespera.bridge.VesperaBridge.configureAsyncMaxInFlight0(int) -> boolean`
///
/// `dispatchAsync` in-flight cap; `0` = unlimited, negative = untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureAsyncMaxInFlight0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_in_flight: jint,
) -> jboolean {
    guarded_configure(|| apply_optional(jlong::from(max_in_flight), set_async_max_in_flight))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::not_configured(-1, true, false)]
    #[case::configured_applied(5, true, true)]
    #[case::configured_rejected(5, false, true)]
    fn optional_values_only_call_the_setter_when_configured(
        #[case] value: jlong,
        #[case] setter_result: bool,
        #[case] expect_call: bool,
    ) {
        let mut called = false;
        let applied = apply_optional(value, |v| {
            called = true;
            assert_eq!(v, 5);
            setter_result
        });
        assert_eq!(called, expect_call);
        assert_eq!(applied, !expect_call || setter_result);
    }

    #[test]
    fn guarded_configure_maps_panics_and_rejections_to_false() {
        assert_eq!(guarded_configure(|| true), JNI_TRUE);
        assert_eq!(guarded_configure(|| false), JNI_FALSE);
        assert_eq!(guarded_configure(|| panic!("boom")), JNI_FALSE);
    }

    #[test]
    fn admission_counts_and_releases_slots() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let first = admit(2, &COUNTER).expect("slot 1");
        let second = admit(2, &COUNTER).expect("slot 2");
        assert!(admit(2, &COUNTER).is_none(), "third call is rejected");
        assert_eq!(
            COUNTER.load(Ordering::Acquire),
            2,
            "a rejection leaks no slot"
        );
        drop(first);
        let third = admit(2, &COUNTER).expect("a released slot is reusable");
        drop((second, third));
        assert_eq!(COUNTER.load(Ordering::Acquire), 0);
        assert!(admit(0, &COUNTER).is_some(), "0 = unlimited");
        assert_eq!(
            COUNTER.load(Ordering::Acquire),
            0,
            "unlimited is not counted"
        );
    }

    #[test]
    fn env_knob_absent_uses_default() {
        assert_eq!(read_env_usize("VESPERA_JNI_TEST_ABSENT_KNOB", 7, 1, 10), 7);
    }

    #[test]
    fn async_overloaded_wire_is_503() {
        let wire = async_overloaded_wire();
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&wire[4..4 + len]).unwrap();
        assert!(header.contains("\"status\":503"), "{header}");
    }
}
