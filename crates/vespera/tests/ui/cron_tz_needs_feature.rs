//! Compile-fail: without the `cron-tz` feature there is no time-zone
//! database, so `tz = "..."` is rejected instead of silently running in UTC.
//!
//! Runs with `cron` but not `cron-tz`.

#[vespera::cron("0 0 9 * * *", tz = "Asia/Seoul")]
pub async fn job() {}

fn main() {}
