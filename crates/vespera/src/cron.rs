//! In-process cron scheduler backing `#[vespera::cron]`.
//!
//! One tokio task per job sleeps until the next UTC occurrence of its
//! expression and spawns the job function, so slow runs may overlap.

use std::{future::Future, pin::Pin};

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

/// Spawn one scheduling task per job on the current tokio runtime.
pub fn start(jobs: impl IntoIterator<Item = CronJob>) {
    for job in jobs {
        tokio::spawn(run_job(job));
    }
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
        tokio::time::sleep((next - Utc::now()).to_std().unwrap_or_default()).await;
        tokio::spawn((job.run)());
        after = next.max(Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    static TICKS: AtomicUsize = AtomicUsize::new(0);

    fn tick() -> JobFuture {
        Box::pin(async {
            TICKS.fetch_add(1, Ordering::SeqCst);
        })
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
}
