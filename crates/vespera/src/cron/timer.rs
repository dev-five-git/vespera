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
}

#[derive(Default)]
struct Timer {
    queue: Mutex<Queue>,
    changed: Condvar,
}

impl Timer {
    fn register(&self, deadline: Instant) -> Arc<Slot> {
        let slot = Arc::new(Slot::default());
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = queue.next_seq;
        queue.next_seq += 1;
        queue.entries.insert((deadline, seq), Arc::clone(&slot));
        drop(queue);
        self.changed.notify_one();
        slot
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

/// The shared timer, its thread started on first use.
fn timer() -> &'static Timer {
    static TIMER: OnceLock<&'static Timer> = OnceLock::new();
    TIMER.get_or_init(|| {
        let timer: &'static Timer = Box::leak(Box::default());
        std::thread::Builder::new()
            .name("vespera-cron-timer".to_owned())
            .spawn(|| timer.run())
            .map_or_else(
                |error| cron_diag!(error, "failed to start the cron timer thread: {error}"),
                drop,
            );
        timer
    })
}

/// Resolve after `duration`, without needing a Tokio time driver.
pub(super) fn sleep(duration: Duration) -> impl Future<Output = ()> {
    let slot = timer().register(Instant::now() + duration);
    poll_fn(move |cx| {
        if slot.fired.load(Ordering::SeqCst) {
            return Poll::Ready(());
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
        let started = Instant::now();
        no_timers.block_on(async {
            let long = sleep(Duration::from_millis(60));
            sleep(Duration::from_millis(20)).await;
            assert!(started.elapsed() >= Duration::from_millis(20));
            long.await;
        });
        assert!(started.elapsed() >= Duration::from_millis(60));
        // A past deadline resolves on the first poll after it fired.
        no_timers.block_on(sleep(Duration::ZERO));
    }
}
