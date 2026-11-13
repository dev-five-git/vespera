//! `vespera_configure_*` — process-wide knobs for C hosts, the C twins of
//! the JNI `configure*0` hooks.  Each one forwards to the existing Rust
//! setter (same clamping, same `0` meaning, same first-wins rule), so a C
//! host no longer has to export `VESPERA_*` environment variables before
//! loading the library.  Precedence stays setter > env > default.
//!
//! Call them **before `vespera_init`**: the runtime knobs are fixed when
//! `vespera_init` starts the shared runtime, the others when a dispatch
//! first reads them.  Each returns `VESPERA_OK` when applied,
//! `VESPERA_ERR_TOO_LATE` when the knob was already fixed (nothing changes),
//! or `VESPERA_ERR_PANIC`.

use std::{
    ffi::c_int,
    panic::{AssertUnwindSafe, catch_unwind},
    time::Duration,
};

use super::{VESPERA_ERR_PANIC, VESPERA_ERR_TOO_LATE, VESPERA_OK};
use crate::runtime::{
    set_async_max_in_flight, set_runtime_max_blocking_threads, set_runtime_worker_threads,
};

/// Run a first-wins setter under a panic guard and map its outcome to a
/// return code.
fn configure(apply: impl FnOnce() -> bool) -> c_int {
    match catch_unwind(AssertUnwindSafe(apply)) {
        Ok(true) => VESPERA_OK,
        Ok(false) => VESPERA_ERR_TOO_LATE,
        Err(_) => VESPERA_ERR_PANIC,
    }
}

/// `int vespera_configure_worker_threads(size_t)` — shared runtime worker
/// threads ([`set_runtime_worker_threads`], clamped to `1..=1024`).
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_worker_threads(worker_threads: usize) -> c_int {
    configure(|| set_runtime_worker_threads(worker_threads))
}

/// `int vespera_configure_max_blocking_threads(size_t)` — shared runtime
/// blocking-pool cap ([`set_runtime_max_blocking_threads`], clamped to
/// `1..=8192`).
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_max_blocking_threads(max_blocking_threads: usize) -> c_int {
    configure(|| set_runtime_max_blocking_threads(max_blocking_threads))
}

/// `int vespera_configure_max_request_bytes(size_t)` — request size cap
/// ([`crate::set_max_request_bytes`]; `0` = unlimited) → `413`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_max_request_bytes(max_bytes: usize) -> c_int {
    configure(|| crate::set_max_request_bytes(max_bytes))
}

/// `int vespera_configure_max_buffered_response_bytes(size_t)` — buffered
/// response cap ([`crate::set_max_buffered_response_bytes`]; `0` =
/// unlimited) → `500`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_max_buffered_response_bytes(max_bytes: usize) -> c_int {
    configure(|| crate::set_max_buffered_response_bytes(max_bytes))
}

/// `int vespera_configure_dispatch_timeout_ms(uint64_t)` — per-dispatch
/// deadline ([`crate::set_dispatch_timeout`]; `0` = disabled) → `504`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_dispatch_timeout_ms(timeout_ms: u64) -> c_int {
    configure(|| crate::set_dispatch_timeout(Duration::from_millis(timeout_ms)))
}

/// `int vespera_configure_async_max_in_flight(size_t)` — cap on in-flight
/// `vespera_dispatch_async` calls, shared with JNI `dispatchAsync`
/// ([`set_async_max_in_flight`]; `0` = unlimited) → `503`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_configure_async_max_in_flight(max_in_flight: usize) -> c_int {
    configure(|| set_async_max_in_flight(max_in_flight))
}

#[cfg(test)]
mod tests {
    //! The guard only; the symbols themselves change process-wide state and
    //! are exercised in their own process (`tests/ffi_configure.rs`).
    use super::*;

    #[test]
    fn configure_maps_outcomes_to_codes() {
        assert_eq!(configure(|| true), VESPERA_OK);
        assert_eq!(configure(|| false), VESPERA_ERR_TOO_LATE);
        assert_eq!(configure(|| panic!("boom")), VESPERA_ERR_PANIC);
    }
}
