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
//!   after the gap (a 02:30 job runs at 03:00 when 02:00 jumps to 03:00),
//!   and runs once in a fall-back overlap (at the first 01:30).
//! - **Overlap.** By default ([`Overlap::Allow`]) every tick starts a run, so
//!   a slow job may overlap itself.  `overlap = "skip"` ([`Overlap::Skip`])
//!   skips — and logs — a tick while the previous run is still going.
//! - **Panics.** Every run is supervised: a panicking job is logged to
//!   stderr (`vespera: cron job `…` panicked: …`) and the schedule keeps
//!   going.  With the `tracing` feature these diagnostics (panic, overlap
//!   skip, unschedulable job, an expression with no occurrence left) are
//!   `tracing` events on target
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
//! with no runtime — likewise `vespera_init` from `ffi_app!`) they are handed
//! to the shared host runtime (`vespera_inprocess::runtime`), so
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
        mpsc::RecvTimeoutError,
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
///
/// The jobs run on the Tokio runtime current at the FIRST call (see
/// [`start`]) and are never restarted: if that runtime is short-lived — a
/// `#[tokio::test]`, or a temporary `Runtime::new()?.block_on(create_app())`
/// used only to dump the OpenAPI document or warm up — the jobs stop with it.
/// Build the app first on the runtime that serves it (or outside any runtime,
/// which uses a dedicated `vespera-cron` thread).
pub fn start_once(guard: &'static Once, jobs: impl IntoIterator<Item = CronJob>) {
    guard.call_once(|| start(jobs));
}

/// Spawn one scheduling task per job.
///
/// Uses the current Tokio runtime when there is one; in a host-loaded (JNI / C ABI)
/// library the jobs wait for the shared host runtime; otherwise starts a
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
    #[cfg(any(feature = "jni", feature = "ffi"))]
    if vespera_inprocess::runtime::host_loaded() {
        vespera_inprocess::runtime::on_shared_runtime(Box::new(move |handle| {
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
/// first (those runs keep going until their runtime stops).  Needs no Tokio
/// time driver: it returns at once when nothing is in flight, and otherwise
/// times out on a short-lived helper thread, so it may be awaited on any
/// executor.
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

    /// `true` once no run is in flight, `false` when `timeout` elapsed first.
    ///
    /// Needs no Tokio time driver (nor a runtime at all): an idle controller
    /// answers at once, and the timeout is a plain thread that wakes the
    /// waiter — it exits as soon as the wait ends either way.
    async fn wait_idle(&self, timeout: Duration) -> bool {
        if self.in_flight.load(Ordering::SeqCst) == 0 {
            return true;
        }
        let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
        let (expire, expired) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            // `Disconnected` (the wait ended) exits without firing.
            if let Err(RecvTimeoutError::Timeout) = cancelled.recv_timeout(timeout) {
                let _ = expire.send(());
            }
        });
        let mut idle = pin!(self.idle());
        let mut expired = pin!(expired);
        let done = poll_fn(|cx| {
            if idle.as_mut().poll(cx).is_ready() {
                return Poll::Ready(true);
            }
            expired.as_mut().poll(cx).map(|_| false)
        })
        .await;
        drop(cancel);
        done
    }

    /// Spawn one supervised run of `job`: the job runs in its own task, and
    /// a supervisor awaits it so a panic is logged instead of vanishing with
    /// a dropped `JoinHandle`.  The returned handle finishes with the run.
    ///
    /// Returns `None` without running anything once shutdown was requested.
    /// The run is counted in flight *before* the stop flag is checked: a
    /// concurrent [`shutdown_and_wait`] (stores the flag, then reads the
    /// count — both `SeqCst`) either sees this run in flight or this check
    /// sees the flag, so it can never report idle while a run starts.
    fn launch(&'static self, job: CronJob) -> Option<JoinHandle<()>> {
        let in_flight = InFlight::enter(self);
        if self.stopped.load(Ordering::SeqCst) {
            // Dropping the guard wakes `idle` waiters when this was the last.
            return None;
        }
        let run = tokio::spawn((job.run)());
        let supervisor = tokio::spawn(async move {
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
        });
        Some(supervisor)
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
    // `enable_all`, not just timers: jobs on this thread commonly reach a
    // database or HTTP client, which panic ("IO is disabled") on a runtime
    // without the I/O driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
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

/// Why a scheduler task ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobEnd {
    /// The expression or time zone was rejected; nothing ever ran.
    Unschedulable,
    /// [`shutdown`] (or the job's own controller) stopped it.
    Stopped,
    /// The expression has no occurrence left (e.g. a past year).
    Exhausted,
}

async fn run_job(job: CronJob, controller: &'static Controller) -> JobEnd {
    let schedule = match Schedule::new(&job) {
        Ok(schedule) => schedule,
        Err(error) => {
            cron_diag!(error, "cron job `{}` not scheduled: {error}", job.name);
            return JobEnd::Unschedulable;
        }
    };
    let mut after = Utc::now();
    let mut running: Vec<JoinHandle<()>> = Vec::new();
    let mut end = JobEnd::Exhausted;
    while let Some(next) = schedule.next_after(after) {
        if controller
            .unless_stopped(sleep_until_wall_clock(next))
            .await
            .is_none()
        {
            end = JobEnd::Stopped;
            break;
        }
        running.retain(|run| !run.is_finished());
        if job.overlap == Overlap::Skip && !running.is_empty() {
            cron_diag!(
                warn,
                "cron job `{}` skipped its {next} run: the previous run is still in progress",
                job.name
            );
        } else if let Some(run) = controller.launch(job) {
            running.push(run);
        }
        // A shutdown that raced the wake-up made `launch` back out; the next
        // `unless_stopped` sees the flag and ends the loop.
        after = next.max(Utc::now());
    }
    if end == JobEnd::Exhausted {
        cron_diag!(
            warn,
            "cron job `{}` stopped: `{}` has no occurrence after {after}",
            job.name,
            job.expression
        );
    }
    // Keep this scheduler alive until its in-flight runs finish, so a
    // dedicated scheduler thread does not drop (cancel) them with its runtime.
    for run in running {
        drop(run.await);
    }
    end
}

/// Longest single Tokio sleep while waiting for an occurrence.
///
/// Tokio sleeps on the monotonic clock, which some OSes pause while the
/// machine is suspended and which ignores wall-clock steps.  Sleeping in
/// slices of at most this long and re-reading the wall clock after each one
/// keeps a run at most one slice late after a suspend or a forward step.
const MAX_SLEEP_SLICE: Duration = Duration::from_secs(60);

/// Sleep until the wall clock reaches `next`, re-reading it at least every
/// [`MAX_SLEEP_SLICE`]: a backwards step (NTP correction, manual change)
/// does not fire the job early, and a forward step or a suspend does not
/// leave it waiting out the remaining monotonic time.
async fn sleep_until_wall_clock(next: DateTime<Utc>) {
    sleep_until_wall_clock_with(next, Utc::now).await;
}

/// [`sleep_until_wall_clock`] reading the wall clock from `now` (the seam
/// tests use to fake a clock jump).
async fn sleep_until_wall_clock_with(next: DateTime<Utc>, mut now: impl FnMut() -> DateTime<Utc>) {
    while let Some(slice) = sleep_slice(next, now()) {
        tokio::time::sleep(slice).await;
    }
}

/// The next sleep: the time left until `next` capped at [`MAX_SLEEP_SLICE`],
/// or `None` once `now` reached it.
fn sleep_slice(next: DateTime<Utc>, now: DateTime<Utc>) -> Option<Duration> {
    remaining_until(next, now).map(|remaining| remaining.min(MAX_SLEEP_SLICE))
}

/// Time left until `next`, or `None` once `now` reached it.
fn remaining_until(next: DateTime<Utc>, now: DateTime<Utc>) -> Option<Duration> {
    (now < next).then(|| (next - now).to_std().unwrap_or_default())
}

#[cfg(test)]
mod tests;
