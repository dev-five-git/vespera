//! Compile-fail: an unknown `tz = "..."` name is a span-attached compile
//! error, checked against the same IANA database the scheduler uses.
//!
//! Requires the `cron-tz` feature.

#[vespera::cron("0 0 9 * * *", tz = "Asia/Seol")]
pub async fn job() {}

fn main() {}
