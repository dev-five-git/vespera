pub mod cleanup;

/// Jobs in an inline module keep its segment (`crate::cron::inline::…`).
pub mod inline {
    use std::sync::atomic::Ordering;

    #[vespera::cron("* * * * * *")]
    pub async fn inline_probe() {
        crate::CRON_PROBE_RUNS[1].fetch_add(1, Ordering::SeqCst);
    }
}

/// Same function name as [`inline::inline_probe`] in a sibling inline module
/// of the same file: both are scheduled, each under its own path.
pub mod inline_twin {
    use std::sync::atomic::Ordering;

    #[vespera::cron("* * * * * *")]
    pub async fn inline_probe() {
        crate::CRON_PROBE_RUNS[3].fetch_add(1, Ordering::SeqCst);
    }
}
