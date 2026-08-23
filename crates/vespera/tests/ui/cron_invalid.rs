//! Compile-fail: a malformed `#[cron("...")]` expression must be a clean,
//! span-attached compile error — not a `JobScheduler` panic at application
//! startup (the pre-fix behaviour, where `Job::new_async(expr).expect(...)`
//! ran only once the app booted).
//!
//! Requires the `cron` feature (which compiles the croner-backed validator
//! into the proc-macro).

#[vespera::cron("not a valid cron expression")]
pub async fn job() {}

fn main() {}
