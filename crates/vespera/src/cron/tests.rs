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
        // Socket I/O panics ("IO is disabled") unless the scheduler
        // thread's runtime enables the I/O driver.
        if tokio::net::TcpListener::bind("127.0.0.1:0").await.is_ok() {
            NO_RUNTIME_TICKS.fetch_add(1, Ordering::SeqCst);
        }
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

/// The daylight-saving behaviour the module docs promise, in
/// America/New_York (2030: spring forward 03-10 02:00 EST -> 03:00 EDT,
/// fall back 11-03 02:00 EDT -> 01:00 EST): the occurrences following
/// `after`, in UTC.
#[cfg(feature = "cron-tz")]
#[rstest]
// 02:30 does not exist on 03-10: the run moves to the first instant
// after the gap (03:00 EDT = 07:00Z), then the next day is 02:30 EDT.
#[case::spring_forward_gap(
    "0 30 2 * * *",
    "2030-03-09T12:00:00Z",
    &["2030-03-10T07:00:00+00:00", "2030-03-11T06:30:00+00:00"]
)]
// 01:30 happens twice on 11-03 (EDT 05:30Z, EST 06:30Z): one run only.
#[case::fall_back_overlap(
    "0 30 1 * * *",
    "2030-11-02T12:00:00Z",
    &["2030-11-03T05:30:00+00:00", "2030-11-04T06:30:00+00:00"]
)]
fn daylight_saving_transitions_follow_the_documented_rules(
    #[case] expression: &'static str,
    #[case] after: &str,
    #[case] expected: &[&str],
) {
    let job = CronJob::new(expression, tick).with_timezone("America/New_York");
    let schedule = Schedule::new(&job).unwrap();
    let mut at = utc(after);
    let runs: Vec<String> = expected
        .iter()
        .map(|_| {
            at = schedule.next_after(at).unwrap();
            at.to_rfc3339()
        })
        .collect();
    assert_eq!(runs, expected);
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
async fn unschedulable_job_returns_without_running(#[case] job: CronJob, #[case] message: &str) {
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

/// A tick whose sleep ended just as `shutdown_and_wait` began reaches
/// `launch` after the flag is set: it backs out instead of starting a run
/// the waiter has already reported idle.
#[tokio::test]
async fn launch_after_shutdown_backs_out_and_stays_idle() {
    let controller = controller();
    controller.shutdown();
    assert!(
        controller
            .launch(CronJob::new("* * * * * *", stopped_tick))
            .is_none()
    );
    assert_eq!(controller.in_flight.load(Ordering::SeqCst), 0);
    assert!(controller.wait_idle(Duration::from_millis(10)).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(STOPPED_TICKS.load(Ordering::SeqCst), 0);
}

/// Outside any runtime (plain `#[test]`, JNI `JNI_OnLoad`) `start` must
/// not panic: the jobs run on the dedicated scheduler thread, whose
/// runtime has the I/O driver (the job binds a socket).
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

/// No sleep is longer than [`MAX_SLEEP_SLICE`]: after each slice the wall
/// clock is read again, so a suspend (which pauses the monotonic clock on
/// some OSes) or a forward clock step delays a run by at most one slice.
#[rstest]
#[case::far_away(chrono::Duration::hours(6), Some(MAX_SLEEP_SLICE))]
#[case::exactly_one_slice(chrono::Duration::seconds(60), Some(MAX_SLEEP_SLICE))]
#[case::under_one_slice(chrono::Duration::seconds(3), Some(Duration::from_secs(3)))]
#[case::reached(chrono::Duration::zero(), None)]
#[case::passed(chrono::Duration::seconds(-1), None)]
fn sleep_slice_is_bounded(
    #[case] until_next: chrono::Duration,
    #[case] expected: Option<Duration>,
) {
    let now = utc("2030-01-01T00:00:00Z");
    assert_eq!(sleep_slice(now + until_next, now), expected);
}

/// A fake wall clock that jumps forward one hour (the machine slept) while
/// the scheduler waits: the wait ends after ONE slice of paused Tokio time,
/// not after the hour the monotonic clock would still have to run.
#[tokio::test(start_paused = true)]
async fn wall_clock_jump_ends_the_wait_after_one_slice() {
    let start = utc("2030-01-01T00:00:00Z");
    let next = start + chrono::Duration::hours(1);
    let mut readings = [start, next].into_iter();
    let began = tokio::time::Instant::now();
    sleep_until_wall_clock_with(next, move || readings.next().unwrap()).await;
    assert_eq!(began.elapsed(), MAX_SLEEP_SLICE);
}

/// Why a scheduler ended: an expression that runs out of occurrences is
/// reported as exhausted (and logged), not ended silently.
#[rstest]
#[case::past_year("0 0 0 1 1 * 1970", JobEnd::Exhausted)]
#[case::invalid("not a cron", JobEnd::Unschedulable)]
#[tokio::test]
async fn run_job_reports_why_it_ended(#[case] expression: &'static str, #[case] expected: JobEnd) {
    assert_eq!(
        run_job(CronJob::new(expression, tick), controller()).await,
        expected
    );
}

#[tokio::test]
async fn run_job_reports_a_stop() {
    let controller = controller();
    controller.shutdown();
    assert_eq!(
        run_job(CronJob::new("* * * * * *", stopped_tick), controller).await,
        JobEnd::Stopped
    );
}

/// Poll `future` to completion on the calling thread with no Tokio context.
fn block_on_without_runtime<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `wait_idle` needs neither a runtime nor its time driver: idle returns at
/// once, and the timeout runs on a plain thread.
#[test]
fn wait_idle_works_without_a_time_driver() {
    let controller = controller();
    let no_timers = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(no_timers.block_on(controller.wait_idle(Duration::from_secs(5))));
    assert!(block_on_without_runtime(
        controller.wait_idle(Duration::from_secs(5))
    ));
    let run = InFlight::enter(controller);
    assert!(!no_timers.block_on(controller.wait_idle(Duration::from_millis(50))));
    assert!(!block_on_without_runtime(
        controller.wait_idle(Duration::from_millis(50))
    ));
    let finisher = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(run);
    });
    assert!(block_on_without_runtime(
        controller.wait_idle(Duration::from_secs(30))
    ));
    finisher.join().unwrap();
}
