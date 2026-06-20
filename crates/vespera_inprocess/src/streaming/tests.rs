use super::{RequestProducerHandle, RequestSourceCloser};
use std::sync::{Arc, Mutex};

/// A panicking user close hook must be CONTAINED by `close_if_started`:
/// the method also runs from `Drop` during unwind, where an escaping panic
/// would be a double-panic → process `abort()`.  Build a "started" producer
/// handle (a real `JoinHandle`, so `producer_was_started` is true and the
/// hook actually runs), then assert the call returns normally despite the
/// hook panicking, and that a second call is a consumed-hook no-op.
///
/// Without the `catch_unwind` in `close_if_started`, the first call would
/// unwind out of this `#[test]` (and, on a real `Drop`-during-unwind path,
/// abort the process).
#[test]
fn close_hook_panic_is_contained() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    // `Runtime::spawn` hands back a live `JoinHandle` without entering the
    // runtime (the empty task is never driven or awaited) — we only need a
    // handle present so the producer counts as "started".
    let join_handle = runtime.spawn(async {});
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(Some(join_handle)));

    let mut closer = RequestSourceCloser::new(Arc::clone(&producer_handle), || panic!("hook boom"));
    // Returns normally — the panic is caught inside `close_if_started`.
    closer.close_if_started();
    // Idempotent: the hook was consumed on the first call, so this is a
    // no-op and does not panic a second time.
    closer.close_if_started();
}
