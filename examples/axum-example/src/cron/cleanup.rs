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
