pub mod cleanup;

/// Jobs in an inline module keep its segment (`crate::cron::inline::…`).
pub mod inline {
    use std::sync::atomic::Ordering;

    #[vespera::cron("* * * * * *")]
    pub async fn inline_probe() {
        crate::CRON_PROBE_RUNS[1].fetch_add(1, Ordering::SeqCst);
    }
}
