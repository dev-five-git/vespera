//! Cron jobs started from a host-loaded library (`JNI_OnLoad`,
//! `vespera_init`) wait for the shared host runtime instead of starting a
//! `vespera-cron` thread, and run on it once it is built.
//!
//! Lives in its own test binary because `mark_host_loaded` is permanent for
//! the process: it would reroute the no-runtime schedulers of any other test
//! sharing the binary.

#![cfg(all(feature = "cron", any(feature = "jni", feature = "ffi")))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vespera::cron::{CronJob, JobFuture};
use vespera::inprocess::runtime;

static TICKS: AtomicUsize = AtomicUsize::new(0);

fn tick() -> JobFuture {
    Box::pin(async {
        TICKS.fetch_add(1, Ordering::SeqCst);
    })
}

#[test]
fn host_loaded_jobs_run_on_the_shared_runtime() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    runtime::mark_host_loaded();
    vespera::cron::start([CronJob::new("* * * * * *", tick)]);
    // Handed to the shared runtime, which is not built yet: nothing runs.
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        TICKS.load(Ordering::SeqCst),
        0,
        "ran before the host runtime"
    );

    assert!(runtime::start_runtime_for_hooks(), "jobs must be waiting");
    let deadline = Instant::now() + Duration::from_secs(5);
    while TICKS.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "job never ran on the shared runtime"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
