//! Cron discovery end to end: jobs at the crate root, in an inline module and
//! after `vespera!` compile into the right paths and are all scheduled.

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use axum_example::{CRON_PROBE_RUNS, create_app};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_jobs_at_root_inline_and_after_vespera_all_run() {
    let _app = create_app().await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let runs: Vec<usize> = CRON_PROBE_RUNS
            .iter()
            .map(|runs| runs.load(Ordering::SeqCst))
            .collect();
        if runs.iter().all(|&runs| runs > 0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "every probe job should run within 10 s, runs = {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
