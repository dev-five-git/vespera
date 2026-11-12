//! `sync_runtime_ab`: same-run A/B for the JNI synchronous dispatch driver.
//!
//! * `current_thread_tls` — the REMOVED design: one current-thread runtime
//!   per calling OS thread (thread-local, blocking pool capped at 4), driven
//!   with `block_on`.
//! * `shared_multi_thread` — the current design: `block_on` on ONE shared
//!   multi-thread runtime from the (non-runtime) calling thread, which is
//!   what `dispatchBytes` / `dispatchDirect0` now do.
//!
//! Both arms dispatch the same wire request through
//! `dispatch_from_bytes_async`, so the delta isolates the driver cost.

// `#[path]` bench submodule of `dispatch.rs`; re-uses the parent imports
// (same convention as the sibling `streaming` module).
#[allow(clippy::wildcard_imports)]
use super::*;

use std::cell::RefCell;

thread_local! {
    static TLS_CURRENT_THREAD: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

fn block_on_tls_current_thread<F: std::future::Future>(future: F) -> F::Output {
    TLS_CURRENT_THREAD.with(|slot| {
        let mut slot = slot.borrow_mut();
        let runtime = slot.get_or_insert_with(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(4)
                .build()
                .expect("current-thread runtime")
        });
        runtime.block_on(future)
    })
}

pub fn bench_sync_runtime_ab(c: &mut Criterion) {
    install_bench_app();

    let shared = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let mut group = c.benchmark_group("sync_runtime_ab");

    for &body_kb in &[0_usize, 1] {
        let payload = vec![0xA5u8; body_kb * 1024];
        let wire = assemble_wire(
            "POST",
            "/echo/bytes",
            Some("application/octet-stream"),
            &payload,
        );

        group.bench_with_input(
            BenchmarkId::new("current_thread_tls", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    block_on_tls_current_thread(vespera_inprocess::dispatch_from_bytes_async(
                        wire.clone(),
                    ))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("shared_multi_thread", body_kb),
            &body_kb,
            |b, _| {
                b.iter(|| {
                    shared.block_on(vespera_inprocess::dispatch_from_bytes_async(wire.clone()))
                });
            },
        );
    }

    group.finish();
    drop(shared);
}
