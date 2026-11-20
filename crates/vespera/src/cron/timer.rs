//! The process-wide timer behind the cron schedulers' sleeps.
//!
//! A scheduler task runs on whatever Tokio runtime started it, and Tokio
//! offers no way to ask whether that runtime has its time driver: building a
//! `tokio::time::Sleep` on a runtime without `enable_time` panics (and under
//! `panic = "abort"` that cannot be caught).  The schedulers therefore never
//! touch Tokio's timer: their sleeps are woken by one lazily started
//! `vespera-cron-timer` thread, which works on any runtime (or executor).

use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    sync::{
        Arc, Condvar, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    time::{Duration, Instant},
};

/// One pending sleep: set once its deadline passed, waking the stored waker.
#[derive(Default)]
struct Slot {
    fired: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Slot {
    fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[derive(Default)]
struct Queue {
    /// Pending sleeps by `(deadline, sequence number)`, earliest first.
    entries: BTreeMap<(Instant, u64), Arc<Slot>>,
    next_seq: u64,
    /// Whether the timer thread has been started.  A failed start leaves it
    /// `false`, so the next [`Timer::ensure_running`] retries it.
    running: bool,
    /// Whether a failed start was already logged.  It is set by the first
    /// failed start and never cleared: the thread, once started, never stops,
    /// so the latch is never consulted again.  While the thread is down, it
    /// keeps every poll of a waiting sleep from writing another diagnostic.
    start_reported: bool,
}

#[derive(Default)]
struct Timer {
    queue: Mutex<Queue>,
    changed: Condvar,
}

/// A sleep's entry in the timer queue.  Dropping it removes the entry, so a
/// sleep dropped early, or one that resolves while the thread is down (and
/// so never fired), does not stay queued until its deadline.
struct Registration {
    timer: &'static Timer,
    key: (Instant, u64),
    slot: Arc<Slot>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.timer.remove(self.key);
    }
}

impl Timer {
    /// Start the timer thread through `spawn` unless it already runs.
    /// Returns whether the thread runs.  A failed start is not cached:
    /// nothing fires until a later call starts the thread, and that call
    /// fires every sleep that came due in the meantime.
    fn ensure_running(
        &'static self,
        spawn: impl FnOnce(&'static Self) -> std::io::Result<()>,
    ) -> bool {
        // Held across the spawn: the new thread's first `run` waits on this
        // lock, so it cannot race the `running` update below.
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.running {
            return true;
        }
        match spawn(self) {
            Ok(()) => {
                queue.running = true;
                true
            }
            Err(error) => {
                if !queue.start_reported {
                    queue.start_reported = true;
                    cron_diag!(error, "failed to start the cron timer thread: {error}");
                }
                false
            }
        }
    }

    fn register(&'static self, deadline: Instant) -> Registration {
        let slot = Arc::new(Slot::default());
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = queue.next_seq;
        queue.next_seq += 1;
        let key = (deadline, seq);
        queue.entries.insert(key, Arc::clone(&slot));
        drop(queue);
        self.changed.notify_one();
        Registration {
            timer: self,
            key,
            slot,
        }
    }

    /// Forget the entry `key` if the timer has not fired it yet.
    fn remove(&self, key: (Instant, u64)) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .remove(&key);
    }

    /// Fire every sleep whose deadline passed, then wait for the next one
    /// (or a new registration).  Never returns.
    fn run(&self) -> ! {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let Some((&(deadline, seq), _)) = queue.entries.first_key_value() else {
                queue = self
                    .changed
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            if deadline <= now {
                let slot = queue.entries.remove(&(deadline, seq));
                drop(queue);
                if let Some(slot) = slot {
                    slot.fire();
                }
                queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            } else {
                queue = self
                    .changed
                    .wait_timeout(queue, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
}

/// The shared timer (created on first use; its thread is started by
/// [`sleep`], see [`Timer::ensure_running`]).
fn timer() -> &'static Timer {
    static TIMER: OnceLock<&'static Timer> = OnceLock::new();
    TIMER.get_or_init(|| -> &'static Timer { Box::leak(Box::default()) })
}

/// Start the process-wide timer thread.
fn spawn_timer_thread(timer: &'static Timer) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("vespera-cron-timer".to_owned())
        .spawn(move || timer.run())
        .map(drop)
}

/// Longest blocking wait between two attempts to start the timer thread.
const RETRY_SLICE: Duration = Duration::from_millis(50);

/// Resolve after `duration`, without needing a Tokio time driver.
pub(super) fn sleep(duration: Duration) -> impl Future<Output = ()> {
    sleep_with(timer(), spawn_timer_thread, duration)
}

/// [`sleep`] on `timer`, starting its thread through `spawn` while it does
/// not run.
///
/// The sleep is registered even while the thread is down, but nothing fires
/// it then.  Each poll retries the start, and while it still fails the future
/// waits in short blocking slices and resolves at its deadline regardless of
/// the thread, so a process at its thread limit never parks a job forever.
/// Once the thread runs, the registered slot fires as usual.  However the
/// sleep ends, its queue entry is removed when the future is dropped.
///
/// Cost while the thread is down: each poll blocks the polling thread for up
/// to `RETRY_SLICE` before returning `Pending` and waking itself, so a
/// current-thread runtime serving a waiting job runs its other tasks only
/// between slices.  No Tokio timer is used, because that would need a probe
/// for the runtime's time driver, which Tokio does not offer.  The failed
/// start is logged once, until the thread starts, not on every poll.
fn sleep_with(
    timer: &'static Timer,
    spawn: fn(&'static Timer) -> std::io::Result<()>,
    duration: Duration,
) -> impl Future<Output = ()> {
    let deadline = Instant::now() + duration;
    let registration = timer.register(deadline);
    poll_fn(move |cx| {
        let slot = &registration.slot;
        if slot.fired.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        if !timer.ensure_running(spawn) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Poll::Ready(());
            }
            std::thread::sleep(left.min(RETRY_SLICE));
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        *slot.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        // Re-check: a `fire` between the first check and storing the waker
        // found no waker to wake.
        if slot.fired.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeps_fire_in_deadline_order_on_a_runtime_without_timers() {
        let no_timers = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        no_timers.block_on(async {
            // Both sleeps run concurrently; the record shows which resolved first.
            let long = {
                let order = Arc::clone(&order);
                tokio::spawn(async move {
                    sleep(Duration::from_millis(60)).await;
                    order.lock().unwrap().push("long");
                })
            };
            let short = {
                let order = Arc::clone(&order);
                tokio::spawn(async move {
                    sleep(Duration::from_millis(20)).await;
                    order.lock().unwrap().push("short");
                })
            };
            short.await.unwrap();
            long.await.unwrap();
        });
        assert_eq!(*order.lock().unwrap(), ["short", "long"]);
        assert!(started.elapsed() >= Duration::from_millis(60));
        // A past deadline resolves on the first poll after it fired.
        no_timers.block_on(sleep(Duration::ZERO));
    }

    /// A thread start that fails is retried by the next call, and the sleep
    /// registered while it was down still fires once the thread runs.
    #[test]
    fn a_failed_timer_thread_start_is_retried_not_cached() {
        let timer: &'static Timer = Box::leak(Box::default());
        let mut attempts = 0;
        assert!(!timer.ensure_running(|_| {
            attempts += 1;
            Err(std::io::Error::other("no threads left"))
        }));
        let registration = timer.register(Instant::now());
        let slot = &registration.slot;
        assert!(timer.ensure_running(|timer| {
            attempts += 1;
            std::thread::Builder::new()
                .spawn(move || timer.run())
                .map(drop)
        }));
        assert_eq!(attempts, 2, "the failed start was retried, not cached");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !slot.fired.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the sleep never fired");
            std::thread::sleep(Duration::from_millis(1));
        }
        // Once running, further calls never spawn again.
        assert!(timer.ensure_running(|_| panic!("the timer thread was started twice")));
    }

    /// A sleep whose thread cannot start (a process at its thread limit)
    /// still resolves at its deadline instead of waiting for a thread that
    /// never comes.
    #[test]
    fn a_sleep_resolves_at_its_deadline_while_the_thread_cannot_start() {
        let timer: &'static Timer = Box::leak(Box::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let started = Instant::now();
        runtime.block_on(sleep_with(
            timer,
            |_| Err(std::io::Error::other("no threads left")),
            Duration::from_millis(30),
        ));
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert!(!timer.queue.lock().unwrap().running);
    }

    /// Repeated failed starts latch the diagnostic: the flag is set by the
    /// first one and, once set, is never cleared, not even by a later
    /// successful start (no per-poll log line).
    #[test]
    fn a_failed_start_latches_its_diagnostic_until_the_thread_starts() {
        let timer: &'static Timer = Box::leak(Box::default());
        assert!(!timer.queue.lock().unwrap().start_reported);
        for _ in 0..3 {
            assert!(!timer.ensure_running(|_| Err(std::io::Error::other("no threads left"))));
        }
        assert!(timer.queue.lock().unwrap().start_reported);
        assert!(!timer.queue.lock().unwrap().running);
        assert!(timer.ensure_running(|_| Ok(())));
        assert!(timer.queue.lock().unwrap().start_reported);
        assert!(timer.queue.lock().unwrap().running);
    }

    /// Sleeps that resolve while the thread is down leave no entry behind, so
    /// an outage does not accumulate one per sleep.
    #[test]
    fn sleeps_resolved_while_the_thread_is_down_leave_no_entry() {
        let timer: &'static Timer = Box::leak(Box::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for _ in 0..5 {
            runtime.block_on(sleep_with(
                timer,
                |_| Err(std::io::Error::other("no threads left")),
                Duration::ZERO,
            ));
        }
        assert!(timer.queue.lock().unwrap().entries.is_empty());
    }

    /// A sleep dropped before its deadline removes its entry at once, not at
    /// the deadline.
    #[test]
    fn a_dropped_sleep_removes_its_entry() {
        let timer: &'static Timer = Box::leak(Box::default());
        let pending = sleep_with(
            timer,
            |_| Err(std::io::Error::other("no threads left")),
            Duration::from_secs(3600),
        );
        assert_eq!(timer.queue.lock().unwrap().entries.len(), 1);
        drop(pending);
        assert!(timer.queue.lock().unwrap().entries.is_empty());
    }
}
