//! Compile-fail: without the `cron` feature `#[vespera::cron("...")]` names
//! the missing feature instead of registering a job that `vespera!` later
//! starts through an unresolved `vespera::cron::start_once`.
//!
//! Runs only when `cron` is off.

#[vespera::cron("0 */5 * * * *")]
pub async fn cleanup() {}

fn main() {
    // The function itself is kept, so references to it still resolve.
    let _ = cleanup;
}
