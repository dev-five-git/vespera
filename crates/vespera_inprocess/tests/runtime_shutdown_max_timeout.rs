//! `shutdown_runtime_then(Duration::MAX, ..)` ("wait as long as it takes")
//! must not panic on `Instant + Duration` overflow after the gate closed:
//! the runtime is shut down and the cleanup runs.  Own process: shutdown is
//! irreversible.
#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vespera_inprocess::runtime::{runtime, shutdown_runtime_then};

static CLEANED: AtomicBool = AtomicBool::new(false);

fn cleanup() {
    CLEANED.store(true, Ordering::SeqCst);
}

#[test]
fn shutdown_with_duration_max_completes_and_runs_cleanup() {
    assert!(runtime().is_some());
    assert!(shutdown_runtime_then(Duration::MAX, cleanup));
    assert!(CLEANED.load(Ordering::SeqCst));
    assert!(runtime().is_none());
}
