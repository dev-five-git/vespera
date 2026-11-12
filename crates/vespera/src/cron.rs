//! In-process cron scheduler backing `#[vespera::cron]`.
//!
//! One tokio task per job sleeps until the next occurrence of its expression
//! and spawns the job function.
//!
//! - **Time zone.** Expressions are evaluated in **UTC** by default.
//!   `#[vespera::cron("0 0 9 * * *", tz = "Asia/Seoul")]` evaluates one in an
//!   IANA time zone instead (requires the `cron-tz` feature; the name is
//!   checked at compile time).  Daylight-saving transitions follow croner:
//!   a fixed-time job whose time falls into a spring-forward gap runs right
//!   after the gap, and runs once in a fall-back overlap.
//! - **Overlap.** By default ([`Overlap::Allow`]) every tick starts a run, so
//!   a slow job may overlap itself.  `overlap = "skip"` ([`Overlap::Skip`])
//!   skips — and logs — a tick while the previous run is still going.
//! - **Panics.** Every run is supervised: a panicking job is logged to
//!   stderr (`vespera: cron job `…` panicked: …`) and the schedule keeps
//!   going.  With the `tracing` feature these diagnostics (panic, overlap
//!   skip, unschedulable job) are `tracing` events on target
//!   `vespera::cron` (ERROR / WARN) instead of `eprintln!`.
//! - **Shutdown.** [`shutdown`] stops scheduling new runs for good;
//!   [`shutdown_and_wait`] additionally waits (bounded) for in-flight runs.
//!   [`Serve::serve_with_graceful_shutdown`](crate::Serve::serve_with_graceful_shutdown)
//!   calls both: new runs stop when the signal fires, and in-flight runs get
//!   up to [`DEFAULT_SHUTDOWN_GRACE`] once the server has drained.
//!
//! The `vespera!`-generated router factory starts its jobs through
//! [`start_once`], so calling the factory repeatedly (tests, `jni_app!`,
//! several `serve` calls) never stacks duplicate schedulers.  Scheduling
//! works with or without an ambient Tokio runtime: inside one the jobs are
//! spawned onto it.  In a JNI library (the factory runs inside `JNI_OnLoad`,
//! with no runtime) they are handed to `vespera_jni`'s shared runtime, so
//! they honour `configureRuntime` and stop with `shutdown0` (which cancels
//! in-flight runs with the runtime).  Anywhere else outside a runtime (a
//! plain `#[test]`) they run on a dedicated `vespera-cron` background thread
//! that owns a current-thread runtime, instead of panicking in
//! `tokio::spawn`.

use std::{
    any::Any,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::{
        Once,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

use chrono::{DateTime, Utc};
use tokio::{sync::Notify, task::JoinHandle};
use vespera_core::cron::{Cron, parse};

/// Scheduler diagnostic: a `tracing` event at `$level` on target
/// `vespera::cron` with the `tracing` feature, else a `vespera: `-prefixed
/// `eprintln!` (the pre-feature behaviour).
#[cfg(feature = "tracing")]
macro_rules! cron_diag {
    ($level:ident, $($arg:tt)+) => {
        ::tracing::$level!(target: "vespera::cron", $($arg)+)
    };
}
#[cfg(not(feature = "tracing"))]
macro_rules! cron_diag {
    ($level:ident, $($arg:tt)+) => {
        ::std::eprintln!("vespera: {}", ::std::format_args!($($arg)+))
    };
}

/// Boxed future returned by a cron job function.
pub type JobFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// How long [`Serve::serve_with_graceful_shutdown`](crate::Serve::serve_with_graceful_shutdown)
/// waits for in-flight cron runs after the server has drained.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// What a tick does while the previous run of the same job is still going.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overlap {
    /// Start another run anyway, so runs may overlap (the default).
    #[default]
    Allow,
    /// Skip the tick and log it; the next tick tries again.
    Skip,
}

/// A cron expression paired with the function it triggers.
#[derive(Clone, Copy, Debug)]
pub struct CronJob {
    expression: &'static str,
    run: fn() -> JobFuture,
    name: &'static str,
    timezone: Option<&'static str>,
    overlap: Overlap,
}

impl CronJob {
    /// A job evaluated in UTC that allows overlapping runs, named after its
    /// expression in logs.
    #[must_use]
    pub const fn new(expression: &'static str, run: fn() -> JobFuture) -> Self {
        Self {
            expression,
            run,
            name: expression,
            timezone: None,
            overlap: Overlap::Allow,
        }
    }

    /// Name the job in scheduler logs (`vespera!` passes `module::function`).
    #[must_use]
    pub const fn with_name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Evaluate the expression in an IANA time zone (`"Asia/Seoul"`) instead
    /// of UTC.  Needs the `cron-tz` feature: an unknown name — or any name
    /// without the feature — leaves the job unscheduled with a logged error.
    #[must_use]
    pub const fn with_timezone(mut self, timezone: &'static str) -> Self {
        self.timezone = Some(timezone);
        self
    }

    /// Choose what a tick does while the previous run is still going.
    #[must_use]
    pub const fn with_overlap(mut self, overlap: Overlap) -> Self {
        self.overlap = overlap;
        self
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
/// After [`shutdown`] the schedulers exit without running anything.
pub fn start(jobs: impl IntoIterator<Item = CronJob>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        for job in jobs {
            handle.spawn(run_job(job, &CONTROLLER));
        }
        return;
    }
    let jobs: Vec<CronJob> = jobs.into_iter().collect();
    #[cfg(feature = "jni")]
    if vespera_jni::jni_loaded() {
        vespera_jni::on_shared_runtime(Box::new(move |handle| {
            for job in jobs {
                handle.spawn(run_job(job, &CONTROLLER));
            }
        }));
        return;
    }
    spawn_scheduler_thread(jobs);
}

/// Stop scheduling new cron runs, process-wide and for good.
///
/// Runs already in progress continue; await [`shutdown_and_wait`] to wait for
/// them.  Idempotent, never blocks, and callable outside a runtime.
pub fn shutdown() {
    CONTROLLER.shutdown();
}

/// [`shutdown`], then wait up to `timeout` for in-flight runs to finish.
///
/// Returns `true` when no run is left, `false` when the timeout elapsed
/// first (those runs keep going until their runtime stops).  Must be awaited
/// inside a Tokio runtime with the time driver enabled.
pub async fn shutdown_and_wait(timeout: Duration) -> bool {
    CONTROLLER.shutdown();
    CONTROLLER.wait_idle(timeout).await
}

/// Process-wide stop flag and in-flight run count.
static CONTROLLER: Controller = Controller::new();

/// Stop signal plus in-flight tracking shared by a set of schedulers.  The
/// process uses [`CONTROLLER`]; tests use their own so stopping one never
/// stops the schedulers of another test running in the same process.
struct Controller {
    stopped: AtomicBool,
    stop: Notify,
    in_flight: AtomicUsize,
    idle: Notify,
}

impl Controller {
    const fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            stop: Notify::const_new(),
            in_flight: AtomicUsize::new(0),
            idle: Notify::const_new(),
        }
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop.notify_waiters();
    }

    /// Resolves once [`Controller::shutdown`] was called.
    async fn stopped(&self) {
        let mut notified = pin!(self.stop.notified());
        // Register before checking the flag, so a concurrent `shutdown`
        // either is seen by the check or wakes the registered waiter.
        notified.as_mut().enable();
        if !self.stopped.load(Ordering::SeqCst) {
            notified.await;
        }
    }

    /// Drive `future` to completion (`Some`) unless shutdown is requested
    /// first (`None`).
    async fn unless_stopped<F: Future>(&self, future: F) -> Option<F::Output> {
        let mut future = pin!(future);
        let mut stopped = pin!(self.stopped());
        poll_fn(|cx| {
            if stopped.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            future.as_mut().poll(cx).map(Some)
        })
        .await
    }

    /// Resolves once no run is in flight.
    async fn idle(&self) {
        loop {
            let mut notified = pin!(self.idle.notified());
            notified.as_mut().enable();
            if self.in_flight.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    async fn wait_idle(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.idle()).await.is_ok()
    }

    /// Spawn one supervised run of `job`: the job runs in its own task, and
    /// a supervisor awaits it so a panic is logged instead of vanishing with
    /// a dropped `JoinHandle`.  The returned handle finishes with the run.
    fn launch(&'static self, job: CronJob) -> JoinHandle<()> {
        let in_flight = InFlight::enter(self);
        let run = tokio::spawn((job.run)());
        tokio::spawn(async move {
            let _in_flight = in_flight;
            if let Err(error) = run.await
                && error.is_panic()
            {
                let payload = error.into_panic();
                cron_diag!(
                    error,
                    "cron job `{}` panicked: {}",
                    job.name,
                    panic_message(&*payload)
                );
            }
        })
    }
}

/// Counts one in-flight run; dropping it (run finished, or its task was
/// cancelled with the runtime) wakes [`Controller::idle`] waiters at zero.
struct InFlight(&'static Controller);

impl InFlight {
    fn enter(controller: &'static Controller) -> Self {
        controller.in_flight.fetch_add(1, Ordering::SeqCst);
        Self(controller)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// The message of a `panic!` payload (`&str` or `String`).
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Run `jobs` on a detached background thread that owns a current-thread
/// runtime for the rest of the process.
fn spawn_scheduler_thread(jobs: Vec<CronJob>) {
    std::thread::Builder::new()
        .name("vespera-cron".to_owned())
        .spawn(move || run_scheduler_thread(jobs))
        .map_or_else(
            |error| cron_diag!(error, "failed to start the cron scheduler thread: {error}"),
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
            .map(|job| tokio::spawn(run_job(job, &CONTROLLER)))
            .collect();
        for scheduler in schedulers {
            drop(scheduler.await);
        }
    });
}

#[cfg(feature = "cron-tz")]
type Zone = vespera_core::cron::Tz;

/// `None` is UTC; a name is looked up in the IANA database.
#[cfg(feature = "cron-tz")]
fn resolve_zone(name: Option<&str>) -> Result<Zone, String> {
    name.map_or(Ok(Zone::UTC), |name| {
        vespera_core::cron::parse_timezone(name).map_err(|_| {
            format!("unknown time zone `{name}` (expected an IANA name such as \"Asia/Seoul\")")
        })
    })
}

#[cfg(not(feature = "cron-tz"))]
type Zone = Utc;

/// Without the `cron-tz` feature only UTC exists.
#[cfg(not(feature = "cron-tz"))]
fn resolve_zone(name: Option<&str>) -> Result<Zone, String> {
    name.map_or(Ok(Utc), |name| {
        Err(format!(
            "time zone `{name}` needs the `cron-tz` feature of vespera"
        ))
    })
}

/// A parsed expression plus the zone it is evaluated in.
struct Schedule {
    cron: Cron,
    zone: Zone,
}

impl Schedule {
    fn new(job: &CronJob) -> Result<Self, String> {
        let cron = parse(job.expression)
            .map_err(|error| format!("invalid cron expression `{}`: {error}", job.expression))?;
        let zone = resolve_zone(job.timezone)?;
        Ok(Self { cron, zone })
    }

    /// The first occurrence strictly after `after`, or `None` when there is
    /// none (e.g. a past year).
    fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.cron
            .find_next_occurrence(&after.with_timezone(&self.zone), false)
            .ok()
            .map(|next| next.with_timezone(&Utc))
    }
}

async fn run_job(job: CronJob, controller: &'static Controller) {
    let schedule = match Schedule::new(&job) {
        Ok(schedule) => schedule,
        Err(error) => {
            cron_diag!(error, "cron job `{}` not scheduled: {error}", job.name);
            return;
        }
    };
    let mut after = Utc::now();
    let mut running: Vec<JoinHandle<()>> = Vec::new();
    while let Some(next) = schedule.next_after(after) {
        if controller
            .unless_stopped(sleep_until_wall_clock(next))
            .await
            .is_none()
        {
            break;
        }
        running.retain(|run| !run.is_finished());
        if job.overlap == Overlap::Skip && !running.is_empty() {
            cron_diag!(
                warn,
                "cron job `{}` skipped its {next} run: the previous run is still in progress",
                job.name
            );
        } else {
            running.push(controller.launch(job));
        }
        after = next.max(Utc::now());
    }
    // Keep this scheduler alive until its in-flight runs finish, so a
    // dedicated scheduler thread does not drop (cancel) them with its runtime.
    for run in running {
        drop(run.await);
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

    use rstest::rstest;

    use super::*;

    static TICKS: AtomicUsize = AtomicUsize::new(0);
    static NO_RUNTIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static ONCE_FIRST_TICKS: AtomicUsize = AtomicUsize::new(0);
    static ONCE_SECOND_TICKS: AtomicUsize = AtomicUsize::new(0);
    static SKIP_STARTS: AtomicUsize = AtomicUsize::new(0);
    static ALLOW_STARTS: AtomicUsize = AtomicUsize::new(0);
    static PANIC_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static DRAIN_STARTED: AtomicUsize = AtomicUsize::new(0);
    static DRAIN_FINISHED: AtomicUsize = AtomicUsize::new(0);
    static HANG_STARTED: AtomicUsize = AtomicUsize::new(0);
    static STOPPED_TICKS: AtomicUsize = AtomicUsize::new(0);

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

    /// Never finishes within a test: the first run is still going on every
    /// later tick.
    fn skip_probe() -> JobFuture {
        Box::pin(async {
            SKIP_STARTS.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    }

    fn allow_probe() -> JobFuture {
        Box::pin(async {
            ALLOW_STARTS.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    }

    fn panicking() -> JobFuture {
        Box::pin(async {
            PANIC_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
            panic!("boom");
        })
    }

    fn draining() -> JobFuture {
        Box::pin(async {
            DRAIN_STARTED.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(400)).await;
            DRAIN_FINISHED.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn hanging() -> JobFuture {
        Box::pin(async {
            HANG_STARTED.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    }

    fn stopped_tick() -> JobFuture {
        Box::pin(async {
            STOPPED_TICKS.fetch_add(1, Ordering::SeqCst);
        })
    }

    /// A private controller per test, so stopping it never stops the
    /// schedulers of another test (or the process-wide `CONTROLLER`).
    fn controller() -> &'static Controller {
        Box::leak(Box::new(Controller::new()))
    }

    fn utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
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

    async fn wait_for_async(counter: &AtomicUsize, at_least: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if counter.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[test]
    fn next_after_is_strictly_after_and_none_for_past_year() {
        let after = utc("2030-06-15T12:00:00Z");
        let yearly = Schedule::new(&CronJob::new("0 0 0 1 1 * *", tick)).unwrap();
        assert_eq!(
            yearly.next_after(after).unwrap().to_rfc3339(),
            "2031-01-01T00:00:00+00:00"
        );
        let past = Schedule::new(&CronJob::new("0 0 0 1 1 * 1970", tick)).unwrap();
        assert!(past.next_after(after).is_none());
    }

    #[test]
    fn job_builders_set_name_timezone_and_overlap() {
        let plain = CronJob::new("0 * * * * *", tick);
        assert_eq!(
            (plain.name, plain.timezone, plain.overlap),
            ("0 * * * * *", None, Overlap::Allow)
        );
        let tuned = plain
            .with_name("tasks::tick")
            .with_timezone("Asia/Seoul")
            .with_overlap(Overlap::Skip);
        assert_eq!(
            (tuned.name, tuned.timezone, tuned.overlap),
            ("tasks::tick", Some("Asia/Seoul"), Overlap::Skip)
        );
        assert_eq!(Overlap::default(), Overlap::Allow);
    }

    /// `tz` shifts the wall-clock fields into the zone, including across a
    /// daylight-saving change.
    #[cfg(feature = "cron-tz")]
    #[rstest]
    #[case::utc_default(None, "2030-06-15T12:00:00Z", "2030-06-16T09:00:00+00:00")]
    #[case::seoul(
        Some("Asia/Seoul"),
        "2030-06-15T12:00:00Z",
        "2030-06-16T00:00:00+00:00"
    )]
    #[case::new_york_summer(
        Some("America/New_York"),
        "2030-06-15T12:00:00Z",
        "2030-06-15T13:00:00+00:00"
    )]
    #[case::new_york_winter(
        Some("America/New_York"),
        "2030-01-15T12:00:00Z",
        "2030-01-15T14:00:00+00:00"
    )]
    fn next_after_evaluates_in_the_job_time_zone(
        #[case] timezone: Option<&'static str>,
        #[case] after: &str,
        #[case] expected: &str,
    ) {
        let job = CronJob::new("0 0 9 * * *", tick);
        let job = timezone.map_or(job, |timezone| job.with_timezone(timezone));
        let schedule = Schedule::new(&job).unwrap();
        assert_eq!(
            schedule.next_after(utc(after)).unwrap().to_rfc3339(),
            expected
        );
    }

    /// A job that cannot be scheduled is reported and returns at once.
    #[rstest]
    #[case::invalid_expression(CronJob::new("not a cron", tick), "invalid cron expression")]
    #[cfg_attr(
        feature = "cron-tz",
        case::unknown_time_zone(
            CronJob::new("* * * * * *", tick).with_timezone("Mars/Olympus_Mons"),
            "unknown time zone `Mars/Olympus_Mons`"
        )
    )]
    #[cfg_attr(
        not(feature = "cron-tz"),
        case::time_zone_without_feature(
            CronJob::new("* * * * * *", tick).with_timezone("Asia/Seoul"),
            "needs the `cron-tz` feature"
        )
    )]
    #[tokio::test]
    async fn unschedulable_job_returns_without_running(
        #[case] job: CronJob,
        #[case] message: &str,
    ) {
        let error = Schedule::new(&job).err().expect("must not schedule");
        assert!(error.contains(message), "{error}");
        tokio::time::timeout(Duration::from_secs(2), run_job(job, controller()))
            .await
            .expect("returns at once");
    }

    #[tokio::test]
    async fn expression_without_future_occurrence_returns() {
        run_job(CronJob::new("0 0 0 1 1 * 1970", tick), controller()).await;
    }

    #[tokio::test]
    async fn every_second_job_fires_repeatedly() {
        start([CronJob::new("* * * * * *", tick)]);
        tokio::time::sleep(Duration::from_millis(2600)).await;
        assert!(TICKS.load(Ordering::SeqCst) >= 2);
    }

    /// `Skip` starts one run while it is still going; `Allow` starts one per
    /// tick.
    #[rstest]
    #[case::skip(Overlap::Skip, skip_probe as fn() -> JobFuture, &SKIP_STARTS)]
    #[case::allow(Overlap::Allow, allow_probe as fn() -> JobFuture, &ALLOW_STARTS)]
    #[tokio::test]
    async fn overlap_policy_controls_concurrent_runs(
        #[case] overlap: Overlap,
        #[case] run: fn() -> JobFuture,
        #[case] starts: &'static AtomicUsize,
    ) {
        let controller = controller();
        let job = CronJob::new("* * * * * *", run).with_overlap(overlap);
        tokio::spawn(run_job(job, controller));
        tokio::time::sleep(Duration::from_millis(3200)).await;
        let started = starts.load(Ordering::SeqCst);
        if overlap == Overlap::Skip {
            assert_eq!(started, 1, "skip must not stack runs");
        } else {
            assert!(started >= 2, "allow must overlap, got {started}");
        }
        controller.shutdown();
    }

    /// A panicking run is logged by its supervisor and the job keeps its
    /// schedule.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panicking_job_is_reported_and_keeps_its_schedule() {
        let controller = controller();
        let job = CronJob::new("* * * * * *", panicking).with_name("tests::panicking");
        let scheduler = tokio::spawn(run_job(job, controller));
        assert!(
            wait_for_async(&PANIC_ATTEMPTS, 2).await,
            "schedule stopped after a panic"
        );
        controller.shutdown();
        tokio::time::timeout(Duration::from_secs(5), scheduler)
            .await
            .expect("scheduler exits on shutdown")
            .expect("scheduler did not panic");
        assert!(controller.wait_idle(Duration::from_secs(5)).await);
    }

    #[rstest]
    #[case::str_payload(Box::new("static message"), "static message")]
    #[case::string_payload(Box::new(String::from("owned message")), "owned message")]
    #[case::other_payload(Box::new(42_u8), "non-string panic payload")]
    fn panic_message_reads_string_payloads(
        #[case] payload: Box<dyn Any + Send>,
        #[case] expected: &str,
    ) {
        assert_eq!(panic_message(&*payload), expected);
    }

    /// Shutdown stops new runs at once; in-flight runs finish and are
    /// awaited, and the scheduler exits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_stops_scheduling_and_drains_in_flight_runs() {
        let controller = controller();
        let scheduler = tokio::spawn(run_job(CronJob::new("* * * * * *", draining), controller));
        assert!(wait_for_async(&DRAIN_STARTED, 1).await, "job never started");
        controller.shutdown();
        assert!(controller.wait_idle(Duration::from_secs(5)).await);
        assert_eq!(DRAIN_FINISHED.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(5), scheduler)
            .await
            .expect("scheduler exits on shutdown")
            .expect("scheduler did not panic");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            DRAIN_STARTED.load(Ordering::SeqCst),
            1,
            "ran after shutdown"
        );
    }

    #[tokio::test]
    async fn wait_idle_times_out_while_a_run_is_in_flight() {
        let controller = controller();
        tokio::spawn(run_job(CronJob::new("* * * * * *", hanging), controller));
        assert!(wait_for_async(&HANG_STARTED, 1).await, "job never started");
        controller.shutdown();
        assert!(!controller.wait_idle(Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn stopped_controller_never_runs_a_job() {
        let controller = controller();
        controller.shutdown();
        tokio::time::timeout(
            Duration::from_secs(2),
            run_job(CronJob::new("* * * * * *", stopped_tick), controller),
        )
        .await
        .expect("returns at once");
        assert_eq!(STOPPED_TICKS.load(Ordering::SeqCst), 0);
        assert!(controller.wait_idle(Duration::from_millis(10)).await);
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
        let next = utc("2030-01-01T00:00:10Z");
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
