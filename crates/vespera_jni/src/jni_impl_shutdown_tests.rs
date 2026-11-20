//! [`shutdown_runtime`] takes the shared runtime down for good: later
//! dispatches find no runtime and answer `503`, and a second shutdown is a
//! no-op.  Runs in a CHILD process because the runtime is process-global.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::routing::get;
use vespera_inprocess::runtime::runtime_built;

use super::{
    RUNTIME_SHUT_DOWN_MSG, block_on_shared_runtime, on_shared_runtime, panic_wire, runtime,
    runtime_unavailable_wire, shutdown_runtime, start_runtime_for_hooks,
};

const APP: &str = "shutdown_test";

fn status_and_body(wire: &[u8]) -> (String, Vec<u8>) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    (
        String::from_utf8_lossy(&wire[4..4 + len]).into_owned(),
        wire[4 + len..].to_vec(),
    )
}

fn request() -> Vec<u8> {
    request_path("/ok")
}

fn request_path(path: &str) -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"GET","path":"{path}","app":"{APP}"}}"#);
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(header.as_bytes());
    wire
}

#[test]
fn panic_wire_flags_the_metadata() {
    let (header, body) = status_and_body(&panic_wire());
    assert!(header.contains(r#""status":500"#), "{header}");
    assert!(header.contains(r#""panicked":"true""#), "{header}");
    assert_eq!(body, b"panic in Rust engine");
}

const CHILD_ENV: &str = "VESPERA_JNI_SHUTDOWN_TEST_CHILD";

/// Whether this process is the isolated child.  The parent re-runs `test`
/// in a child process (with [`CHILD_ENV`] set) and asserts it passed.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(CHILD_ENV).is_some() {
        return true;
    }
    let output =
        std::process::Command::new(std::env::current_exe().expect("current unit-test executable"))
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated shutdown child test");
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "isolated shutdown test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// A hook registered after shutdown never runs.  A flag, not a panic: the
/// hook runs on the caller's thread when it is (wrongly) flushed, and a
/// panicking hook is caught and only logged.
fn hook_registered_after_shutdown_never_runs() {
    let ran_after_shutdown = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran_after_shutdown);
    on_shared_runtime(Box::new(move |_| seen.store(true, Ordering::SeqCst)));
    assert!(!start_runtime_for_hooks());
    assert!(
        !ran_after_shutdown.load(Ordering::SeqCst),
        "a hook registered after shutdown never runs"
    );
}

#[test]
fn shutdown_makes_later_dispatches_answer_503() {
    if !in_isolated_child("jni_impl::shutdown_tests::shutdown_makes_later_dispatches_answer_503") {
        return;
    }

    // `/slow` reports that it started, i.e. that its dispatch holds a lease.
    let (slow_started_tx, slow_started_rx) = std::sync::mpsc::channel::<()>();
    vespera_inprocess::register_app_named(APP, move || {
        let slow_started_tx = slow_started_tx.clone();
        vespera_inprocess::Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/slow",
                get(move || {
                    let slow_started_tx = slow_started_tx.clone();
                    async move {
                        let _ = slow_started_tx.send(());
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        "slow"
                    }
                }),
            )
    });
    // Nothing waits for the runtime yet, so nothing builds it: it stays lazy.
    assert!(!start_runtime_for_hooks());
    assert!(
        !runtime_built(),
        "no hook is pending, so the runtime is not built"
    );
    // A hook registered before the runtime exists runs once it is built.
    let (hook_tx, hook_rx) = std::sync::mpsc::channel();
    let hook_tx_later = hook_tx.clone();
    on_shared_runtime(Box::new(move |handle| {
        drop(handle.spawn(async move { hook_tx.send("early").unwrap() }));
    }));
    on_shared_runtime(Box::new(|_| panic!("a panicking hook is contained")));
    assert!(
        !runtime_built(),
        "a hook registration does not build the runtime"
    );
    assert!(start_runtime_for_hooks());
    assert!(runtime_built());
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(5)), Ok("early"));
    // ... and one registered afterwards runs at once.
    on_shared_runtime(Box::new(move |handle| {
        drop(handle.spawn(async move { hook_tx_later.send("late").unwrap() }));
    }));
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(5)), Ok("late"));
    // The runtime is up here, so this does not exercise a build failure: it
    // only pins that the unavailable wire of a runtime that is not shut down
    // is the `500` (a shut-down runtime answers `503`).
    let (header, _) = status_and_body(&runtime_unavailable_wire());
    assert!(
        header.contains(r#""status":500"#),
        "unavailable outside a shutdown is a 500: {header}"
    );
    let (header, body) = status_and_body(
        &block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(request()))
            .expect("runtime available before shutdown"),
    );
    assert!(header.contains(r#""status":200"#), "{header}");
    assert_eq!(body, b"ok");

    // A dispatch in flight when the shutdown starts is drained, not cut.
    let slow = std::thread::spawn(|| {
        block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(request_path(
            "/slow",
        )))
    });
    // Wait for the handler itself, not a fixed delay: a slow CI host could
    // otherwise start the shutdown before the dispatch has taken its lease.
    slow_started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("slow handler started");
    assert!(shutdown_runtime(Duration::from_secs(5)));
    let (header, body) = status_and_body(
        &slow
            .join()
            .expect("slow dispatch thread")
            .expect("leased before the shutdown"),
    );
    assert!(header.contains(r#""status":200"#), "{header}");
    assert_eq!(body, b"slow");
    assert!(runtime().is_none());
    hook_registered_after_shutdown_never_runs();
    // A real dispatch after the shutdown never runs: no runtime, no lease.
    assert!(
        block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(request())).is_none()
    );
    let (header, body) = status_and_body(&runtime_unavailable_wire());
    assert!(header.contains(r#""status":503"#), "{header}");
    assert_eq!(body, RUNTIME_SHUT_DOWN_MSG.as_bytes());
    assert!(
        !shutdown_runtime(Duration::ZERO),
        "second shutdown is a no-op"
    );

    overflow_after_shutdown_is_not_stashed();
    fetch_after_shutdown_retries_with_a_larger_buffer();
}

/// A `fetch0` after the shutdown answers the `503` wire.  A buffer too small
/// for it gets `-(len)` (the caller retries with a larger one), never
/// `Integer.MIN_VALUE`, which reads as "expired": the retry reads the same
/// `503` again.
fn fetch_after_shutdown_retries_with_a_larger_buffer() {
    let wire = runtime_unavailable_wire();
    let len = i32::try_from(wire.len()).unwrap();
    let mut small = [0xAAu8; 16];
    assert_eq!(
        super::direct::stash::fetch_into(1, &mut small),
        -len,
        "a too-small buffer asks to retry"
    );
    assert_eq!(small[..9], [0; 9], "the handle area is zeroed");
    let mut big = vec![0u8; wire.len()];
    assert_eq!(super::direct::stash::fetch_into(1, &mut big), len);
    let (header, body) = status_and_body(&big);
    assert!(header.contains(r#""status":503"#), "{header}");
    assert_eq!(body, RUNTIME_SHUT_DOWN_MSG.as_bytes());
}

/// An overflow stashed once the shutdown has cleared the stash (its drain
/// timed out on the dispatch) is not kept: nothing would release it.  The
/// caller gets the retryable `503` instead of a handle.
fn overflow_after_shutdown_is_not_stashed() {
    let pending = match tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(vespera_inprocess::dispatch_into_async_borrowed_resumable(
            &[0, 0, 0, 1, b'{'],
            &mut [],
        )) {
        vespera_inprocess::ResumableDirectWrite::Overflow(pending) => pending,
        vespera_inprocess::ResumableDirectWrite::Complete(_) => panic!("an empty out overflows"),
    };
    let mut out = vec![0u8; 1024];
    let code = super::direct::stash::stash_overflow(&mut out, pending);
    assert_eq!(
        super::direct::stash::stashed_count(),
        0,
        "not stashed after shutdown"
    );
    let len = usize::try_from(code).expect("a complete wire");
    let (header, _) = status_and_body(&out[..len]);
    assert!(header.contains(r#""status":503"#), "{header}");
}

/// A DIRECT overflow during the shutdown's drain is stashed: the dispatch
/// still holds its lease, and the stash closes only in the cleanup that runs
/// after the drain.  Once that cleanup has run, an overflow answers the
/// retryable `503` instead of a handle.  Runs in a CHILD process: the runtime
/// and the stash are process-global.
#[test]
fn overflow_during_the_drain_is_stashed_until_the_clear() {
    if !in_isolated_child(
        "jni_impl::shutdown_tests::overflow_during_the_drain_is_stashed_until_the_clear",
    ) {
        return;
    }
    let (leased_tx, leased_rx) = std::sync::mpsc::channel::<()>();
    // The dispatch leases the runtime and overflows only once the gate is
    // closed, i.e. while the shutdown is draining it.
    let holder = std::thread::spawn(move || {
        block_on_shared_runtime(async move {
            leased_tx
                .send(())
                .expect("the test thread waits for the lease");
            while !vespera_inprocess::runtime::runtime_shut_down() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let pending = match vespera_inprocess::dispatch_into_async_borrowed_resumable(
                &[0, 0, 0, 1, b'{'],
                &mut [],
            )
            .await
            {
                vespera_inprocess::ResumableDirectWrite::Overflow(pending) => pending,
                vespera_inprocess::ResumableDirectWrite::Complete(_) => {
                    panic!("an empty out overflows")
                }
            };
            let mut out = vec![0u8; 1024];
            let code = super::direct::stash::stash_overflow(&mut out, pending);
            (code, out)
        })
    });
    leased_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the dispatch holds its lease");
    let shutdown = std::thread::spawn(|| shutdown_runtime(Duration::from_secs(30)));
    let (code, out) = holder
        .join()
        .expect("holder thread")
        .expect("leased before the shutdown");
    assert!(code < 0, "an overflow: {code}");
    let token = u64::from_be_bytes(out[..8].try_into().unwrap());
    assert_ne!(token, 0, "a handle while the shutdown drains");
    assert!(shutdown.join().expect("shutdown thread"));
    assert_eq!(
        super::direct::stash::stashed_count(),
        0,
        "the clear after the drain drops the stashed response"
    );

    // After the clear the stash is closed: the same overflow is a 503.
    let pending = match tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(vespera_inprocess::dispatch_into_async_borrowed_resumable(
            &[0, 0, 0, 1, b'{'],
            &mut [],
        )) {
        vespera_inprocess::ResumableDirectWrite::Overflow(pending) => pending,
        vespera_inprocess::ResumableDirectWrite::Complete(_) => panic!("an empty out overflows"),
    };
    let mut after = vec![0u8; 1024];
    let code = super::direct::stash::stash_overflow(&mut after, pending);
    let len = usize::try_from(code).expect("a complete 503 wire");
    let (header, _) = status_and_body(&after[..len]);
    assert!(header.contains(r#""status":503"#), "{header}");
    assert_eq!(super::direct::stash::stashed_count(), 0);
}
