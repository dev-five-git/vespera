//! Shared host runtime (`feature = "runtime"`): knobs, deferred hooks,
//! leases, then the irreversible shutdown — one sequential scenario in its
//! own process because every piece of state is process-global.
#![cfg(feature = "runtime")]

use std::sync::mpsc;
use std::time::Duration;

use vespera_inprocess::runtime::{
    RUNTIME_SHUT_DOWN_MSG, block_on_shared_runtime, host_loaded, mark_host_loaded,
    on_shared_runtime, runtime, runtime_built, runtime_max_blocking_threads, runtime_shut_down,
    runtime_unavailable_wire, runtime_worker_threads, set_runtime_max_blocking_threads,
    set_runtime_worker_threads, shutdown_runtime, start_runtime_for_hooks,
};

#[test]
fn lifecycle_from_lazy_build_to_shutdown() {
    assert!(!host_loaded());
    mark_host_loaded();
    assert!(host_loaded());

    assert!(set_runtime_worker_threads(2));
    assert!(!set_runtime_worker_threads(3), "first-wins");
    assert_eq!(runtime_worker_threads(), Some(2));
    // The blocking-pool cap is left unset here: building the runtime must
    // fix it (asserted below), which a knob set beforehand could not show.

    // Hooks registered before the runtime exists wait for it, and nothing
    // here builds it: the runtime stays lazy until a dispatch or a pending
    // hook asks for it.
    assert!(!start_runtime_for_hooks());
    assert!(!runtime_built());
    let (tx, rx) = mpsc::channel::<&str>();
    let early = tx.clone();
    on_shared_runtime(Box::new(move |_| early.send("early").unwrap()));
    on_shared_runtime(Box::new(|_| panic!("a panicking hook is contained")));
    assert!(rx.try_recv().is_err());
    assert!(
        !runtime_built(),
        "registering a hook must not build the runtime"
    );
    assert!(start_runtime_for_hooks());
    assert!(runtime_built());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("early"));
    // ... and run at once afterwards.
    on_shared_runtime(Box::new(move |_| tx.send("late").unwrap()));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("late"));

    let built_with = runtime_max_blocking_threads();
    assert!(
        !set_runtime_max_blocking_threads(built_with % 8192 + 1),
        "an unset knob is fixed once the runtime is built"
    );
    assert_eq!(runtime_max_blocking_threads(), built_with);
    assert_eq!(block_on_shared_runtime(async { 7 }), Some(7));

    // A lease held elsewhere keeps the shutdown drain waiting.
    let lease = runtime().expect("runtime is up");
    let task = lease.handle().spawn(async { 1 });
    assert_eq!(lease.block_on(task).unwrap(), 1);
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let holder_released = std::sync::Arc::clone(&released);
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        // Flag first, then the drop: a shutdown that returns before the holder
        // reaches this point sees the flag clear. The window between the store
        // and the drop is not observable from outside; the lease-count drain
        // in `runtime.rs` is what closes it.
        holder_released.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(lease);
    });
    assert!(!runtime_shut_down());
    assert!(shutdown_runtime(Duration::from_secs(10)));
    assert!(
        released.load(std::sync::atomic::Ordering::SeqCst),
        "shutdown returned while the lease was still held"
    );
    holder.join().unwrap();

    assert!(runtime_shut_down());
    assert!(runtime().is_none());
    assert!(block_on_shared_runtime(async {}).is_none());
    assert!(runtime_unavailable_wire().ends_with(RUNTIME_SHUT_DOWN_MSG.as_bytes()));
    let hook_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&hook_ran);
    on_shared_runtime(Box::new(move |_| {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    assert!(!start_runtime_for_hooks());
    assert!(!shutdown_runtime(Duration::ZERO));
    assert!(
        !hook_ran.load(std::sync::atomic::Ordering::SeqCst),
        "a hook registered after shutdown must never run"
    );
}
