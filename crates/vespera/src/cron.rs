//! In-process cron scheduler backing `#[vespera::cron]`.
//!
//! One tokio task per job sleeps until the next UTC occurrence of its
//! expression and spawns the job function, so slow runs may overlap.
//!
//! The `vespera!`-generated router factory starts its jobs through
//! [`start_once`], so calling the factory repeatedly (tests, `jni_app!`,
//! several `serve` calls) never stacks duplicate schedulers.  Scheduling
//! works with or without an ambient Tokio runtime: inside one the jobs are
//! spawned onto it.  In a JNI library (the factory runs inside `JNI_OnLoad`,
//! with no runtime) they are handed to `vespera_jni`'s shared runtime, so
//! they honour `configureRuntime` and stop with `shutdown0`.  Anywhere else
//! outside a runtime (a plain `#[test]`) they run on a dedicated
//! `vespera-cron` background thread that owns a current-thread runtime,
//! instead of panicking in `tokio::spawn`.

use std::{future::Future, pin::Pin, sync::Once};

use chrono::{DateTime, Utc};
use vespera_core::cron::{Cron, parse};

/// Boxed future returned by a cron job function.
pub type JobFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A cron expression paired with the function it triggers.
#[derive(Clone, Copy, Debug)]
pub struct CronJob {
    expression: &'static str,
    run: fn() -> JobFuture,
}

impl CronJob {
    #[must_use]
    pub const fn new(expression: &'static str, run: fn() -> JobFuture) -> Self {
        Self { expression, run }
    }
}

/// Start the schedulers for `jobs` at most once per `guard`.
///
/// `vespera!` emits one `static` guard per invocation, so each app's jobs are
/// scheduled once per process no matter how often its router is built.
pub fn start_once(guard: &'static Once, jobs: impl IntoIterator<Item = CronJob>) {
    guard.call_once(|| start(jobs));
}

/// Spawn one scheduling task per job.
///
/// Uses the current Tokio runtime when there is one; in a JVM-loaded JNI
/// library the jobs wait for the shared JNI runtime; otherwise starts a
/// dedicated `vespera-cron` thread running a current-thread runtime, so this
/// never panics outside a runtime.  Every call starts a new set of
/// schedulers — prefer [`start_once`] for anything that can run twice.
pub fn start(jobs: impl IntoIterator<Item = CronJob>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        for job in jobs {
            handle.spawn(run_job(job));
        }
        return;
    }
    let jobs: Vec<CronJob> = jobs.into_iter().collect();
    #[cfg(feature = "jni")]
    if vespera_jni::jni_loaded() {
        vespera_jni::on_shared_runtime(Box::new(move |handle| {
            for job in jobs {
                handle.spawn(run_job(job));
            }
        }));
        return;
    }
    spawn_scheduler_thread(jobs);
}

/// Run `jobs` on a detached background thread that owns a current-thread
/// runtime for the rest of the process.
fn spawn_scheduler_thread(jobs: Vec<CronJob>) {
    std::thread::Builder::new()
        .name("vespera-cron".to_owned())
        .spawn(move || run_scheduler_thread(jobs))
        .map_or_else(
            |error| eprintln!("vespera: failed to start the cron scheduler thread: {error}"),
            drop,
        );
}

fn run_scheduler_thread(jobs: Vec<CronJob>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("vespera: cron scheduler runtime");
    runtime.block_on(async move {
        let schedulers: Vec<_> = jobs
            .into_iter()
            .map(|job| tokio::spawn(run_job(job)))
            .collect();
        for scheduler in schedulers {
            drop(scheduler.await);
        }
    });
}

fn next_occurrence(cron: &Cron, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    cron.find_next_occurrence(&after, false).ok()
}

async fn run_job(job: CronJob) {
    let cron = match parse(job.expression) {
        Ok(cron) => cron,
        Err(error) => {
            eprintln!(
                "vespera: invalid cron expression `{}`: {error}",
                job.expression
            );
            return;
        }
    };
    let mut after = Utc::now();
    while let Some(next) = next_occurrence(&cron, after) {
        sleep_until_wall_clock(next).await;
        tokio::spawn((job.run)());
        after = next.max(Utc::now());
    }
}

/// Sleep until the wall clock reaches `next`.  Tokio sleeps on the monotonic
/// clock, so a wall-clock step backwards during the sleep (NTP correction,
/// manual change) would otherwise fire the job early: re-check and sleep
/// again until `Utc::now() >= next`.
async fn sleep_until_wall_clock(next: DateTime<Utc>) {
    while let Some(remaining) = remaining_until(next, Utc::now()) {
        tokio::time::sleep(remaining).await;
    }
}

/// Time left until `next`, or `None` once `now` reached it.
fn remaining_until(next: DateTime<Utc>, now: DateTime<Utc>) -> Option<std::time::Duration> {
    (now < next).then(|| (next - now).to_std().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    use super::*;

    static TICKS: AtomicUsize = AtomicUsize::new(0);
    static NO_RUNTIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static ONCE_FIRST_TICKS: AtomicUsize = AtomicUsize::new(0);
    static ONCE_SECOND_TICKS: AtomicUsize = AtomicUsize::new(0);

    fn tick() -> JobFuture {
        Box::pin(async {
            TICKS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn no_runtime_tick() -> JobFuture {
        Box::pin(async {
            NO_RUNTIME_TICKS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn once_first_tick() -> JobFuture {
        Box::pin(async {
            ONCE_FIRST_TICKS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn once_second_tick() -> JobFuture {
        Box::pin(async {
            ONCE_SECOND_TICKS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn wait_for(counter: &AtomicUsize, at_least: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if counter.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn next_occurrence_is_strictly_after_and_none_for_past_year() {
        let cron = parse("0 0 0 1 1 * *").unwrap();
        let after = DateTime::parse_from_rfc3339("2030-06-15T12:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            next_occurrence(&cron, after).unwrap().to_rfc3339(),
            "2031-01-01T00:00:00+00:00"
        );
        let past = parse("0 0 0 1 1 * 1970").unwrap();
        assert!(next_occurrence(&past, after).is_none());
    }

    #[tokio::test]
    async fn invalid_expression_returns_without_scheduling() {
        run_job(CronJob::new("not a cron", tick)).await;
    }

    #[tokio::test]
    async fn expression_without_future_occurrence_returns() {
        run_job(CronJob::new("0 0 0 1 1 * 1970", tick)).await;
    }

    #[tokio::test]
    async fn every_second_job_fires_repeatedly() {
        start([CronJob::new("* * * * * *", tick)]);
        tokio::time::sleep(Duration::from_millis(2600)).await;
        assert!(TICKS.load(Ordering::SeqCst) >= 2);
    }

    /// Outside any runtime (plain `#[test]`, JNI `JNI_OnLoad`) `start` must
    /// not panic: the jobs run on the dedicated scheduler thread.
    #[test]
    fn start_without_runtime_uses_background_thread() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        start([CronJob::new("* * * * * *", no_runtime_tick)]);
        assert!(wait_for(&NO_RUNTIME_TICKS, 1), "job never fired");
    }

    /// The scheduler thread drains its scheduler tasks and exits once every
    /// job has run out of occurrences.
    #[test]
    fn scheduler_thread_returns_when_jobs_finish() {
        run_scheduler_thread(vec![
            CronJob::new("0 0 0 1 1 * 1970", tick),
            CronJob::new("not a cron", tick),
        ]);
    }

    /// A second `start_once` with the same guard is a no-op, so re-building
    /// the router never stacks duplicate schedulers.
    #[test]
    fn start_once_schedules_a_guard_only_once() {
        static GUARD: Once = Once::new();
        start_once(&GUARD, [CronJob::new("* * * * * *", once_first_tick)]);
        start_once(&GUARD, [CronJob::new("* * * * * *", once_second_tick)]);
        assert!(wait_for(&ONCE_FIRST_TICKS, 1), "first job never fired");
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(ONCE_SECOND_TICKS.load(Ordering::SeqCst), 0);
    }

    /// The wall clock is re-checked after every sleep: a backwards step
    /// leaves time remaining, so the job is not fired early.
    #[test]
    fn remaining_until_tracks_the_wall_clock() {
        let next = DateTime::parse_from_rfc3339("2030-01-01T00:00:10Z")
            .unwrap()
            .to_utc();
        let before = next - chrono::Duration::seconds(3);
        assert_eq!(remaining_until(next, before), Some(Duration::from_secs(3)));
        assert_eq!(remaining_until(next, next), None);
        assert_eq!(
            remaining_until(next, next + chrono::Duration::seconds(1)),
            None
        );
    }

    #[tokio::test]
    async fn sleep_until_a_past_instant_returns_at_once() {
        let started = Instant::now();
        sleep_until_wall_clock(Utc::now() - chrono::Duration::seconds(1)).await;
        sleep_until_wall_clock(Utc::now() + chrono::Duration::milliseconds(20)).await;
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
