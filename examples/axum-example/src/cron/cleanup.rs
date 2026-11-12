/// Cleanup expired sessions every 10 seconds
#[vespera::cron("1/10 * * * * *")]
pub async fn cleanup_sessions() {
    println!("[cron] Running cleanup_sessions job");
}

/// Health check every 30 seconds
#[vespera::cron("1/30 * * * * *")]
pub async fn health_check() {
    println!("[cron] Running health_check job");
}

/// Daily report at 09:00 Seoul time (expressions are UTC unless `tz` is set;
/// `tz` needs the `cron-tz` feature).  `overlap = "skip"` skips a tick while
/// the previous run is still going.
#[vespera::cron("0 0 9 * * *", tz = "Asia/Seoul", overlap = "skip")]
pub async fn daily_report() {
    println!("[cron] Running daily_report job");
}
