//! Compile-fail: `overlap` only accepts "allow" (default) or "skip".

#[vespera::cron("0 * * * * *", overlap = "queue")]
pub async fn job() {}

fn main() {}
