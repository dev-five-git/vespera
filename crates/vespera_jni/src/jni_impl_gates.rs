//! The `dispatchAsync` admission protocol of the JNI layer, kept free of JNI
//! and Tokio so it can be model-checked with [loom]:
//!
//! - [`InFlightCap`]: the `dispatchAsync` concurrency cap
//!   (`vespera.async.maxInFlight`): optimistic `fetch_add`, back out on
//!   overshoot.  Never admits more than `max` permits at once; an overshoot is
//!   transient, so a concurrent caller may be refused (a retryable `503`)
//!   even though, a moment later, a slot would have been free.
//!
//! The shared runtime's lease gate (in-flight count + shut-down flag in one
//! word) lives next to the runtime, in `vespera_inprocess::runtime`
//! (`runtime/gate.rs`), with its own loom models.
//!
//! Under `--cfg vespera_loom` the atomics come from `loom` (a custom cfg
//! rather than `loom` itself: Tokio and other dependencies change behaviour
//! under `cfg(loom)` and expect their own loom dev-dependency).  Only the
//! `loom_*` tests are meaningful in that build.
//!
//! [loom]: https://docs.rs/loom

#[cfg(vespera_loom)]
use loom::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(vespera_loom))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Concurrency cap for `dispatchAsync`: a counter plus the admit/release
/// protocol around it.
pub struct InFlightCap {
    count: AtomicUsize,
}

impl InFlightCap {
    #[cfg(not(vespera_loom))]
    pub const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    #[cfg(vespera_loom)]
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
    pub fn in_flight(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

/// One admitted slot of an [`InFlightCap`]; released on drop.
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
    }
}

/// loom models: `RUSTFLAGS="--cfg vespera_loom" cargo test --release -p
/// vespera_jni --lib loom_` (CI bounds `LOOM_MAX_PREEMPTIONS`).
#[cfg(all(test, vespera_loom))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::AtomicUsize;
    use loom::thread;

    use super::*;

    /// The `dispatchAsync` cap never has more than `max` permits live at
    /// once, and every permit / refusal returns its count.
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
