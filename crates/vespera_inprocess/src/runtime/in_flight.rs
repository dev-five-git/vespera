//! The async-dispatch admission cap shared by every foreign host — JNI
//! `dispatchAsync` (`vespera_jni`) and the C ABI's `vespera_dispatch_async`
//! ([`crate::ffi`]) draw from ONE counter:
//!
//! - [`InFlightCap`]: optimistic `fetch_add`, back out on overshoot.  Never
//!   admits more than `max` permits at once; an overshoot is transient, so a
//!   concurrent caller may be refused (a retryable `503`) even though, a
//!   moment later, a slot would have been free.  Kept free of Tokio so it can
//!   be model-checked with [loom].
//! - [`async_max_in_flight`] / [`set_async_max_in_flight`]: the cap
//!   (`vespera.async.maxInFlight` / `VESPERA_ASYNC_MAX_IN_FLIGHT` /
//!   `vespera_configure_async_max_in_flight`), first-wins.
//! - [`try_acquire_async_permit`] / [`async_overloaded_wire`]: admit one
//!   async dispatch, or the `503` to answer instead.
//!
//! Under `--cfg vespera_loom` the unit-test build takes its atomics from
//! `loom` (see `gate.rs`); only the `loom_*` tests are meaningful there, and
//! a non-test build (this crate as a dependency) always uses `std` atomics.
//!
//! [loom]: https://docs.rs/loom

use std::sync::OnceLock;

#[cfg(all(test, vespera_loom))]
use loom::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(all(test, vespera_loom)))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default cap on concurrently in-flight async dispatches.
///
/// An async dispatch returns immediately, so a caller that fires it in a
/// loop without its own backpressure would otherwise queue unbounded tasks
/// (and their request buffers) on the runtime.  4096 is far above realistic
/// in-process concurrency for a host (servlet pools are typically a few
/// hundred threads) yet bounds a runaway producer; excess calls complete
/// immediately with a retryable `503` wire response.  `0` disables the cap.
pub const DEFAULT_ASYNC_MAX_IN_FLIGHT: usize = 4096;
const MAX_ASYNC_MAX_IN_FLIGHT: usize = 1 << 24;

/// `503` body for an async dispatch rejected by the in-flight cap.
pub const ASYNC_OVERLOADED_MSG: &str = "too many in-flight async dispatches \
     (vespera.async.maxInFlight / VESPERA_ASYNC_MAX_IN_FLIGHT)";

static ASYNC_MAX_IN_FLIGHT: OnceLock<usize> = OnceLock::new();
/// The async in-flight counter + admission protocol (loom-checked).
#[cfg(not(all(test, vespera_loom)))]
static ASYNC_IN_FLIGHT: InFlightCap = InFlightCap::new();
/// loom atomics are not `const`-constructible; the loom build only runs the
/// `loom_*` models, which never touch the static.
#[cfg(all(test, vespera_loom))]
static ASYNC_IN_FLIGHT: std::sync::LazyLock<InFlightCap> =
    std::sync::LazyLock::new(InFlightCap::new);

/// Cap on concurrently in-flight async dispatches (JNI + C ABI together).
/// Precedence: [`set_async_max_in_flight`], then
/// `VESPERA_ASYNC_MAX_IN_FLIGHT`, then [`DEFAULT_ASYNC_MAX_IN_FLIGHT`]
/// (4096).  `0` = unlimited.
#[must_use]
pub fn async_max_in_flight() -> usize {
    *ASYNC_MAX_IN_FLIGHT.get_or_init(|| {
        crate::config::read_env_clamped(
            "VESPERA_ASYNC_MAX_IN_FLIGHT",
            DEFAULT_ASYNC_MAX_IN_FLIGHT,
            0,
            MAX_ASYNC_MAX_IN_FLIGHT,
        )
    })
}

/// Override the async in-flight cap **before the first async dispatch**.
/// `0` = unlimited.  Returns `false` when already fixed.
pub fn set_async_max_in_flight(max: usize) -> bool {
    ASYNC_MAX_IN_FLIGHT
        .set(max.min(MAX_ASYNC_MAX_IN_FLIGHT))
        .is_ok()
}

/// Async dispatches currently holding an in-flight slot (admitted and not
/// yet completed, cancelled or dropped).  Always `0` while the cap is
/// disabled (`0` = unlimited admits without counting).  A diagnostic: a
/// cancelled dispatch must hand its slot back promptly.
#[must_use]
pub fn async_in_flight() -> usize {
    ASYNC_IN_FLIGHT.in_flight()
}

/// One admitted async-dispatch slot; releases it on drop (task completion,
/// a caught panic, or the runtime dropping the task at shutdown).
pub type AsyncPermit = CapPermit;

/// Admit one async dispatch, or `None` when the in-flight cap is reached
/// (the caller answers [`async_overloaded_wire`] immediately).
#[must_use]
pub fn try_acquire_async_permit() -> Option<AsyncPermit> {
    ASYNC_IN_FLIGHT.admit(async_max_in_flight())
}

/// `503` wire response for an async dispatch rejected by the in-flight cap.
#[must_use]
pub fn async_overloaded_wire() -> Vec<u8> {
    crate::error_wire(503, ASYNC_OVERLOADED_MSG)
}

/// Concurrency cap: a counter plus the admit/release protocol around it.
pub struct InFlightCap {
    count: AtomicUsize,
}

impl InFlightCap {
    /// An empty cap (no permit held).
    #[cfg(not(all(test, vespera_loom)))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    #[cfg(all(test, vespera_loom))]
    pub fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    /// Admit one call under `max` (`0` = unlimited, never counted), or
    /// `None` when `max` permits are already held.
    pub fn admit(&'static self, max: usize) -> Option<CapPermit> {
        if max == 0 {
            return Some(CapPermit { cap: None });
        }
        let previous = self.count.fetch_add(1, Ordering::AcqRel);
        if previous >= max {
            self.count.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(CapPermit { cap: Some(self) })
    }

    /// Permits currently counted (transient overshoots included).
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

#[cfg(not(all(test, vespera_loom)))]
impl Default for InFlightCap {
    fn default() -> Self {
        Self::new()
    }
}

/// One admitted slot of an [`InFlightCap`]; released on drop.
#[must_use = "the permit holds its slot only while it is alive"]
pub struct CapPermit {
    /// `None` when admitted uncapped.
    cap: Option<&'static InFlightCap>,
}

impl Drop for CapPermit {
    fn drop(&mut self) {
        if let Some(cap) = self.cap {
            cap.count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(all(test, not(vespera_loom)))]
mod tests {
    use super::*;

    #[test]
    fn cap_counts_and_releases_slots() {
        static CAP: InFlightCap = InFlightCap::new();
        let first = CAP.admit(2).expect("slot 1");
        let second = CAP.admit(2).expect("slot 2");
        assert!(CAP.admit(2).is_none(), "third call is rejected");
        assert_eq!(CAP.in_flight(), 2, "a rejection leaks no slot");
        drop(first);
        let third = CAP.admit(2).expect("a released slot is reusable");
        drop((second, third));
        assert_eq!(CAP.in_flight(), 0);
        assert!(CAP.admit(0).is_some(), "0 = unlimited");
        assert_eq!(CAP.in_flight(), 0, "unlimited is not counted");
        assert_eq!(InFlightCap::default().in_flight(), 0);
    }

    #[test]
    fn async_overloaded_wire_is_503() {
        let wire = async_overloaded_wire();
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&wire[4..4 + len]).unwrap();
        assert!(header.contains("\"status\":503"), "{header}");
        assert!(wire.ends_with(ASYNC_OVERLOADED_MSG.as_bytes()));
    }
}

/// loom models: `RUSTFLAGS="--cfg vespera_loom" cargo test --release -p
/// vespera_inprocess --features runtime --lib loom_` (CI bounds
/// `LOOM_MAX_PREEMPTIONS`).
#[cfg(all(test, vespera_loom))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::AtomicUsize;
    use loom::thread;

    use super::*;

    /// The async cap never has more than `max` permits live at once, and
    /// every permit / refusal returns its count.
    #[test]
    fn loom_cap_never_exceeds_max_and_never_leaks() {
        loom::model(|| {
            const MAX: usize = 1;
            let cap: &'static InFlightCap = Box::leak(Box::new(InFlightCap::new()));
            let live = Arc::new(AtomicUsize::new(0));
            let caller = |live: Arc<AtomicUsize>| {
                thread::spawn(move || {
                    if let Some(permit) = cap.admit(MAX) {
                        let holders = live.fetch_add(1, Ordering::SeqCst) + 1;
                        assert!(holders <= MAX, "{holders} permits live, cap {MAX}");
                        live.fetch_sub(1, Ordering::SeqCst);
                        drop(permit);
                    }
                })
            };
            let a = caller(Arc::clone(&live));
            let b = caller(Arc::clone(&live));
            caller(live).join().unwrap();
            a.join().unwrap();
            b.join().unwrap();
            assert_eq!(cap.in_flight(), 0, "a permit or refusal leaked a slot");
        });
    }
}
