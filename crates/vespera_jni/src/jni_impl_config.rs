//! Runtime / streaming configuration JNI hooks.
//!
//! These symbols are seeded from `VesperaBridge.init()` **before the first
//! dispatch** and then fixed for the process lifetime.  They are split out of
//! `jni_impl.rs` (which owns the per-request dispatch symbols) so each file
//! keeps a single concern — and stays within the 1000-line source cap.

use jni::EnvUnowned;
use jni::objects::JClass;
use jni::sys::jint;

use super::guard_void_symbol;

const MIN_RUNTIME_WORKERS: usize = 1;
const MAX_RUNTIME_WORKERS: usize = 1024;

static RUNTIME_WORKER_THREADS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

/// Worker thread count for the shared [`RUNTIME`](super::RUNTIME), resolved once
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
    // Defensive panic guard: this body cannot panic today, but it is
    // an `extern "system"` JNI symbol, so guard it through the shared
    // `guard_void_symbol` helper (single source of truth for the
    // void-symbol panic policy — every other void JNI symbol uses it).
    // The returned `bool` (panic-caught flag) is intentionally
    // discarded: defense-in-depth, matching the dispatch symbols.
    let _ = guard_void_symbol(|| {
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
    // Defensive panic guard — see `configureRuntime0`: keep every JNI
    // `extern "system"` symbol panic-safe through the shared
    // `guard_void_symbol` helper even though this body cannot panic
    // with the current setters.
    let _ = guard_void_symbol(|| {
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
