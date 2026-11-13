//! `shutdown_runtime_then` called from inside a Tokio runtime (a host
//! completion running on a worker) refuses dispatches at once and finishes
//! the drain + cleanup on a background thread; a never-started runtime is
//! never built.  Own process: shutdown is irreversible.
#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vespera_inprocess::runtime::{runtime, shutdown_runtime_then};

static CLEANED: AtomicBool = AtomicBool::new(false);

fn cleanup() {
    CLEANED.store(true, Ordering::SeqCst);
}

#[test]
fn shutdown_inside_a_runtime_finishes_in_the_background() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(rt.block_on(async { shutdown_runtime_then(Duration::from_secs(1), cleanup) }));
    assert!(runtime().is_none());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !CLEANED.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "background shutdown never ran the cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
