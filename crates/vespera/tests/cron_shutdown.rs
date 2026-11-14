//! Process-wide cron shutdown, wired into
//! [`Serve::serve_with_graceful_shutdown`](vespera::Serve::serve_with_graceful_shutdown).
//!
//! Lives in its own test binary because `vespera::cron::shutdown` is
//! permanent for the process: it would stop the schedulers of any other test
//! sharing the binary.

#![cfg(feature = "cron")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vespera::Serve;
use vespera::axum::Router;
use vespera::cron::{CronJob, JobFuture};

static STARTED: AtomicUsize = AtomicUsize::new(0);
static FINISHED: AtomicUsize = AtomicUsize::new(0);
static LATE: AtomicUsize = AtomicUsize::new(0);

fn slow_job() -> JobFuture {
    Box::pin(async {
        STARTED.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(700)).await;
        FINISHED.fetch_add(1, Ordering::SeqCst);
    })
}

fn late_job() -> JobFuture {
    Box::pin(async {
        LATE.fetch_add(1, Ordering::SeqCst);
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_serve_stops_cron_and_waits_for_in_flight_runs() {
    vespera::cron::start([CronJob::new("* * * * * *", slow_job).with_name("slow_job")]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while STARTED.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "cron job never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Bound here and handed over: no bind-drop-rebind port race.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    // The signal fires at once: the server drains immediately, then the
    // in-flight cron run is awaited before the graceful serve returns.
    Router::new()
        .serve_listener_with_graceful_shutdown(listener, async {})
        .await
        .expect("serve");
    let started = STARTED.load(Ordering::SeqCst);
    assert_eq!(
        FINISHED.load(Ordering::SeqCst),
        started,
        "in-flight run was not awaited"
    );

    // Nothing is scheduled any more — neither the old job nor a new one.
    vespera::cron::start([CronJob::new("* * * * * *", late_job)]);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        STARTED.load(Ordering::SeqCst),
        started,
        "ran after shutdown"
    );
    assert_eq!(LATE.load(Ordering::SeqCst), 0, "started after shutdown");

    // Idempotent, and nothing is left in flight.
    vespera::cron::shutdown();
    assert!(vespera::cron::shutdown_and_wait(Duration::from_millis(10)).await);
}
