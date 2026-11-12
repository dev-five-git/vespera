//! The two hand-rolled atomic admission protocols of the JNI layer, kept
//! free of JNI and Tokio so they can be model-checked with [loom]:
//!
//! - [`LeaseGate`]: the shared runtime's in-flight dispatch count and its
//!   shut-down flag, packed into **one** atomic word.  A dispatch enters with
//!   a single `fetch_add` that also observes the flag; shutdown closes with a
//!   `fetch_or`.  Both RMWs hit the same location, so coherence alone orders
//!   them: either the dispatch saw the flag (and backs out) or the close came
//!   later in that word's modification order and every drain read sees the
//!   dispatch counted.  No `SeqCst` store→load ordering across two variables
//!   is needed (loom models `SeqCst` accesses only as `AcqRel`, so a two-word
//!   Dekker protocol could not be checked there).
//! - [`InFlightCap`]: the `dispatchAsync` concurrency cap
//!   (`vespera.async.maxInFlight`): optimistic `fetch_add`, back out on
//!   overshoot.  Never admits more than `max` permits at once; an overshoot is
//!   transient, so a concurrent caller may be refused (a retryable `503`)
//!   even though, a moment later, a slot would have been free.
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

/// Shut-down bit of [`LeaseGate`]'s state word; the low bits count leases.
const CLOSED: usize = 1 << (usize::BITS - 1);

/// In-flight lease count + shut-down flag in one atomic word.
pub struct LeaseGate {
    state: AtomicUsize,
}

impl LeaseGate {
    #[cfg(not(vespera_loom))]
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    #[cfg(vespera_loom)]
    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Count one lease, or refuse (`false`, nothing counted) once closed.
    pub fn try_enter(&self) -> bool {
        // The RMW's position in the word's modification order decides the
        // race with `close`; `Acquire` orders a refused dispatch after it.
        if self.state.fetch_add(1, Ordering::Acquire) & CLOSED == 0 {
            return true;
        }
        self.exit();
        false
    }

    /// Release one lease counted by [`LeaseGate::try_enter`].  `Release` so a
    /// drain that observes the count reach zero also observes the work the
    /// lease covered.
    pub fn exit(&self) {
        self.state.fetch_sub(1, Ordering::Release);
    }

    /// Close the gate: later [`LeaseGate::try_enter`] calls are refused.
    /// Returns `false` when it was already closed.
    pub fn close(&self) -> bool {
        self.state.fetch_or(CLOSED, Ordering::AcqRel) & CLOSED == 0
    }

    /// Whether [`LeaseGate::close`] ran.
    pub fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED != 0
    }

    /// Live leases (claims included).  `Acquire`: see [`LeaseGate::exit`].
    pub fn in_flight(&self) -> usize {
        self.state.load(Ordering::Acquire) & !CLOSED
    }

    /// Count a lease WITHOUT checking the flag (tests: hold the drain open).
    #[cfg(test)]
    pub fn force_enter(&self) {
        self.state.fetch_add(1, Ordering::Acquire);
    }
}

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
    fn gate_counts_refuses_after_close_and_closes_once() {
        let gate = LeaseGate::new();
        assert!(gate.try_enter());
        assert_eq!(gate.in_flight(), 1);
        assert!(!gate.is_closed());
        assert!(gate.close());
        assert!(!gate.close(), "second close reports already closed");
        assert!(gate.is_closed());
        assert!(!gate.try_enter(), "closed gate refuses");
        assert_eq!(gate.in_flight(), 1, "a refusal leaves no count behind");
        gate.exit();
        assert_eq!(gate.in_flight(), 0);
        assert!(gate.is_closed(), "draining keeps the flag");
    }

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
    use loom::cell::UnsafeCell;
    use loom::sync::Arc;
    use loom::sync::atomic::AtomicUsize;
    use loom::thread;

    use super::*;

    /// Wait (yielding to the model scheduler) until no lease is live.
    fn drain(gate: &LeaseGate) {
        while gate.in_flight() != 0 {
            thread::yield_now();
        }
    }

    /// The shutdown contract: a dispatch is either refused or fully
    /// covered by the drain — the work it did under its lease
    /// happens-before the drain completing.  loom's `UnsafeCell` reports a
    /// data race if the drain could finish while an admitted dispatch is
    /// still (or invisibly) writing.  Two dispatchers, one cell each.
    #[test]
    fn loom_drain_waits_for_every_admitted_dispatch() {
        loom::model(|| {
            let gate = Arc::new(LeaseGate::new());
            let cells = [
                Arc::new(UnsafeCell::new(0_u32)),
                Arc::new(UnsafeCell::new(0_u32)),
            ];

            let dispatchers = cells.clone().map(|work| {
                let gate = Arc::clone(&gate);
                thread::spawn(move || {
                    if !gate.try_enter() {
                        return false;
                    }
                    // SAFETY: loom-checked — no other access may race this.
                    work.with_mut(|w| unsafe { *w = 1 });
                    gate.exit();
                    true
                })
            });

            assert!(gate.close());
            drain(&gate);
            // After the drain no admitted dispatch may still be running, so
            // these reads must not race with the writes above.
            // SAFETY: loom-checked, as above.
            let seen = cells.map(|work| work.with(|w| unsafe { *w }));

            for (dispatcher, seen) in dispatchers.into_iter().zip(seen) {
                let admitted = dispatcher.join().unwrap();
                assert_eq!(
                    seen,
                    u32::from(admitted),
                    "drain missed an admitted dispatch"
                );
            }
            assert!(!gate.try_enter(), "closed gate refuses");
            assert_eq!(gate.in_flight(), 0, "no lease leaks");
        });
    }

    /// Two dispatchers racing two closers: exactly one close wins, every
    /// refusal backs out its count, and the word ends at closed + 0.
    #[test]
    fn loom_racing_enters_and_closes_leave_no_count() {
        loom::model(|| {
            let gate = Arc::new(LeaseGate::new());
            let spawn_dispatch = |gate: Arc<LeaseGate>| {
                thread::spawn(move || {
                    if gate.try_enter() {
                        gate.exit();
                    }
                })
            };
            let a = spawn_dispatch(Arc::clone(&gate));
            let closer = {
                let gate = Arc::clone(&gate);
                thread::spawn(move || gate.close())
            };
            let first = gate.close();
            a.join().unwrap();
            let second = closer.join().unwrap();
            assert!(first ^ second, "exactly one close reports the transition");
            assert!(gate.is_closed());
            assert_eq!(gate.in_flight(), 0);
        });
    }

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
