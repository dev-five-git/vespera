//! Back-off for empty request-body pulls on the bidirectional producer.
//!
//! `RequestChunk::Data(empty)` means "nothing right now, keep the stream
//! open". The first [`SPIN_PULLS`] consecutive empty pulls only yield the
//! thread; after that the producer sleeps with a doubling delay (capped at
//! [`MAX_BACKOFF`]) so a source that is slow to produce does not peg a
//! blocking-pool core. The body is aborted only as a last resort, once the
//! source has stayed silent for [`MAX_EMPTY_WAIT`] of back-off: the clock
//! starts at the first sleep, after the [`SPIN_PULLS`] yields.

use std::time::{Duration, Instant};

/// Consecutive empty pulls answered with a bare `yield_now` before backing off.
pub(super) const SPIN_PULLS: u32 = 1024;
/// First sleep after the spin phase; doubles on every further empty pull.
const MIN_BACKOFF: Duration = Duration::from_millis(1);
/// Longest single sleep between two empty pulls.
const MAX_BACKOFF: Duration = Duration::from_millis(10);
/// Wall time a source may stay silent (empty pulls only) once the back-off
/// sleeps have started, before the request body is aborted with a
/// `StreamAbort`.  The clock starts at the first sleep, not the first empty
/// pull (the spin phase before it is not counted).
pub(super) const MAX_EMPTY_WAIT: Duration = Duration::from_secs(10);

/// Per-body state of the empty-pull back-off.
pub(super) struct EmptyPullBackoff {
    budget: Duration,
    pulls: u32,
    sleep: Duration,
    /// Set at the first sleep; the budget is measured from there.
    since: Option<Instant>,
}

impl EmptyPullBackoff {
    pub(super) fn new(budget: Duration) -> Self {
        Self {
            budget,
            pulls: 0,
            sleep: MIN_BACKOFF,
            since: None,
        }
    }

    /// Account for one empty pull and wait before the next one. Returns
    /// `false` once the source has been silent for the whole budget (the
    /// caller aborts the body); `true` means keep pulling.
    pub(super) fn on_empty(&mut self) -> bool {
        self.pulls = self.pulls.saturating_add(1);
        if self.pulls <= SPIN_PULLS {
            std::thread::yield_now();
            return true;
        }
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() >= self.budget {
            return false;
        }
        std::thread::sleep(self.sleep);
        self.sleep = (self.sleep * 2).min(MAX_BACKOFF);
        true
    }

    /// Non-empty data arrived: the silence is over, start again from scratch.
    pub(super) fn on_data(&mut self) {
        *self = Self::new(self.budget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spin_phase_never_sleeps_or_gives_up() {
        let mut backoff = EmptyPullBackoff::new(Duration::ZERO);
        for _ in 0..SPIN_PULLS {
            assert!(backoff.on_empty());
        }
        assert!(
            backoff.since.is_none(),
            "no sleep before the spin phase ends"
        );
    }

    #[test]
    fn budget_exhaustion_aborts_after_backing_off() {
        let mut backoff = EmptyPullBackoff::new(Duration::from_millis(5));
        let mut kept_open = 0u32;
        while backoff.on_empty() {
            kept_open += 1;
            assert!(kept_open < 100_000, "budget must eventually run out");
        }
        assert!(
            kept_open > SPIN_PULLS,
            "the spin phase is followed by sleeps"
        );
        assert!(backoff.since.is_some());
    }

    #[test]
    fn data_restarts_the_silence_budget() {
        let mut backoff = EmptyPullBackoff::new(Duration::from_millis(5));
        for _ in 0..=SPIN_PULLS {
            assert!(backoff.on_empty());
        }
        assert!(backoff.since.is_some());
        backoff.on_data();
        assert_eq!(backoff.pulls, 0);
        assert!(backoff.since.is_none());
        assert!(backoff.on_empty(), "a fresh budget keeps the stream open");
    }
}
