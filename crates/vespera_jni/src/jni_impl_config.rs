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

use std::time::Duration;

use jni::EnvUnowned;
use jni::objects::JClass;
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jbyteArray, jint, jlong};

use super::{guard_void_symbol, set_runtime_max_blocking_threads, set_runtime_worker_threads};

// The `dispatchAsync` in-flight cap is shared with the C ABI's
// `vespera_dispatch_async` (one counter for both hosts) and lives next to the
// shared runtime, with its loom model (`vespera_inprocess::runtime`).
pub use vespera_inprocess::runtime::{
    DEFAULT_ASYNC_MAX_IN_FLIGHT, async_in_flight, async_max_in_flight, async_overloaded_wire,
    set_async_max_in_flight, try_acquire_async_permit,
};

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

/// `com.devfive.vespera.bridge.VesperaNativeConfig.configureMaxRequestProducers0(long) -> boolean`
///
/// Cap on concurrently running bidirectional-streaming upload producers
/// ([`vespera_inprocess::set_max_streaming_request_producers`]); `0` =
/// unlimited, negative = untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaNativeConfig_configureMaxRequestProducers0<
    'local,
>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    max_producers: jlong,
) -> jboolean {
    guarded_configure(|| {
        apply_optional(
            max_producers,
            vespera_inprocess::set_max_streaming_request_producers,
        )
    })
}

/// `com.devfive.vespera.bridge.VesperaNativeConfig.startRuntime0() -> boolean`
///
/// Called by `VesperaBridge.init()` right after the runtime knobs were
/// applied: builds the shared runtime now when work waits for it (cron jobs
/// registered from `JNI_OnLoad` — see [`super::on_shared_runtime`]), so they
/// start without waiting for the first dispatch.  Otherwise the runtime stays
/// lazy.  Returns whether it was started.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaNativeConfig_startRuntime0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jboolean {
    guarded_configure(super::start_runtime_for_hooks)
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

/// `com.devfive.vespera.bridge.VesperaBridge.shutdown0(long) -> boolean`
///
/// Gracefully shuts the shared Tokio runtime down
/// ([`super::shutdown_runtime`]): new dispatches answer `503` at once,
/// in-flight dispatches of every mode are drained for up to `timeout_millis`
/// (negative = `0`), then the runtime is shut down with the remaining time —
/// a `dispatchAsync` still pending then completes its future with a `503`
/// wire, cron jobs stop.  Called from a Tokio worker thread the drain runs
/// in the background and the call returns immediately.  Irreversible.
/// Returns `false` when it had already been shut down.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_shutdown0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    timeout_millis: jlong,
) -> jboolean {
    guarded_configure(|| {
        super::shutdown_runtime(Duration::from_millis(
            u64::try_from(timeout_millis).unwrap_or(0),
        ))
    })
}

/// Version of this crate (`CARGO_PKG_VERSION`) as reported to the Java
/// bridge by [`Java_com_devfive_vespera_bridge_VesperaBridge_nativeVersion0`].
pub const NATIVE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `com.devfive.vespera.bridge.VesperaBridge.nativeVersion0() -> byte[]`
///
/// UTF-8 bytes of [`NATIVE_VERSION`]; `VesperaBridge.init()` compares it with
/// the bridge JAR's own version (a `major.minor` mismatch fails the init).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_nativeVersion0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jbyteArray {
    super::guard_byte_array_symbol(&mut unowned_env, |env| {
        super::byte_array_or_empty(env, NATIVE_VERSION.as_bytes())
    })
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
    fn native_version_is_the_crate_version() {
        assert_eq!(NATIVE_VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(NATIVE_VERSION.split('.').count(), 3, "{NATIVE_VERSION}");
    }
}
