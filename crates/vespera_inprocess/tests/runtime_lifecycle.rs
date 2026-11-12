//! Shared host runtime (`feature = "runtime"`): knobs, deferred hooks,
//! leases, then the irreversible shutdown — one sequential scenario in its
//! own process because every piece of state is process-global.
#![cfg(feature = "runtime")]

use std::sync::mpsc;
use std::time::Duration;

use vespera_inprocess::runtime::{
    RUNTIME_SHUT_DOWN_MSG, block_on_shared_runtime, host_loaded, mark_host_loaded,
    on_shared_runtime, runtime, runtime_max_blocking_threads, runtime_shut_down,
    runtime_unavailable_wire, runtime_worker_threads, set_runtime_max_blocking_threads,
    set_runtime_worker_threads, shutdown_runtime, start_runtime_for_hooks,
};

#[test]
fn lifecycle_from_lazy_build_to_shutdown() {
    assert!(!host_loaded());
    mark_host_loaded();
    assert!(host_loaded());

    assert!(set_runtime_worker_threads(2));
    assert!(set_runtime_max_blocking_threads(0));
    assert_eq!(runtime_worker_threads(), Some(2));
    assert_eq!(runtime_max_blocking_threads(), 1);

    // Hooks registered before the runtime exists wait for it ...
    assert!(!start_runtime_for_hooks());
    let (tx, rx) = mpsc::channel::<&str>();
    let early = tx.clone();
    on_shared_runtime(Box::new(move |_| early.send("early").unwrap()));
    on_shared_runtime(Box::new(|_| panic!("a panicking hook is contained")));
    assert!(rx.try_recv().is_err());
    assert!(start_runtime_for_hooks());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("early"));
    // ... and run at once afterwards.
    on_shared_runtime(Box::new(move |_| tx.send("late").unwrap()));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("late"));

    assert!(
        !set_runtime_worker_threads(8),
        "fixed once the runtime is built"
    );
    assert!(!set_runtime_max_blocking_threads(8));
    assert_eq!(block_on_shared_runtime(async { 7 }), Some(7));

    // A lease held elsewhere keeps the shutdown drain waiting.
    let lease = runtime().expect("runtime is up");
    let task = lease.handle().spawn(async { 1 });
    assert_eq!(lease.block_on(task).unwrap(), 1);
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(lease);
    });
    assert!(!runtime_shut_down());
    assert!(shutdown_runtime(Duration::from_secs(10)));
    holder.join().unwrap();

    assert!(runtime_shut_down());
    assert!(runtime().is_none());
    assert!(block_on_shared_runtime(async {}).is_none());
    assert!(runtime_unavailable_wire().ends_with(RUNTIME_SHUT_DOWN_MSG.as_bytes()));
    on_shared_runtime(Box::new(|_| panic!("never runs after shutdown")));
    assert!(!start_runtime_for_hooks());
    assert!(!shutdown_runtime(Duration::ZERO));
}
