//! `shutdown_runtime_then` called from inside a Tokio runtime (a host
//! completion running on a worker) refuses dispatches at once and finishes
//! the drain + cleanup on a background thread; afterwards `runtime()` hands
//! out no lease.  Own process: shutdown is irreversible.
#![cfg(feature = "runtime")]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vespera_inprocess::runtime::{runtime, shutdown_runtime_then};

static CLEANED: AtomicBool = AtomicBool::new(false);
static CLEANUP_THREAD: Mutex<Option<String>> = Mutex::new(None);

fn cleanup() {
    *CLEANUP_THREAD.lock().unwrap() = std::thread::current().name().map(str::to_owned);
    CLEANED.store(true, Ordering::SeqCst);
}

#[test]
fn shutdown_inside_a_runtime_finishes_in_the_background() {
    // An in-flight lease the drain must wait for: it is released only after
    // `shutdown_runtime_then` has returned, so a drain run inline on the
    // calling worker would block until the timeout and run the cleanup
    // before returning.
    let lease = runtime().expect("the shared runtime builds");
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        released.recv().unwrap();
        drop(lease);
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        assert!(shutdown_runtime_then(Duration::from_secs(5), cleanup));
        assert!(
            !CLEANED.load(Ordering::SeqCst),
            "the cleanup ran before the in-flight lease was released"
        );
        assert!(runtime().is_none());
    });
    release.send(()).unwrap();
    holder.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !CLEANED.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "background shutdown never ran the cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        CLEANUP_THREAD.lock().unwrap().as_deref(),
        Some("vespera-shutdown")
    );
}
