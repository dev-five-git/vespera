//! Sync dispatch (`dispatchBytes` / `dispatchDirect0`) runs on the SHARED
//! multi-thread runtime via [`block_on_shared_runtime`]:
//!
//! * a runtime-bound resource created during one sync dispatch (here: a
//!   detached actor task reached through a channel) keeps working for a later
//!   sync dispatch issued from a DIFFERENT OS thread;
//! * a detached `tokio::spawn` makes progress after the dispatch returned,
//!   without any further dispatch driving the runtime.
//!
//! Under the removed per-thread current-thread runtime both failed: the actor
//! lived on the first thread's runtime and only ran while that thread was
//! inside `block_on`, so the second thread's request hung (bounded here by
//! the dispatch deadline → `504`).
//!
//! Runs in a CHILD process: the runtime and its config are process-global
//! `OnceLock`s, and `runtime_config_tests` pins the worker count in-process.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::routing::get;
use tokio::sync::{mpsc, oneshot};

use super::{block_on_shared_runtime, set_runtime_worker_threads};

const APP: &str = "shared_runtime_test";

type Ask = oneshot::Sender<u64>;
static ACTOR: OnceLock<mpsc::UnboundedSender<Ask>> = OnceLock::new();
static DETACHED_RAN: AtomicBool = AtomicBool::new(false);

/// First dispatch: spawn a detached actor (a runtime-bound resource) plus a
/// detached timer task, then return immediately.
async fn start() -> &'static str {
    let (tx, mut rx) = mpsc::unbounded_channel::<Ask>();
    tokio::spawn(async move {
        let mut served = 0u64;
        while let Some(reply) = rx.recv().await {
            served += 1;
            let _ = reply.send(served);
        }
    });
    let _ = ACTOR.set(tx);
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        DETACHED_RAN.store(true, Ordering::Release);
    });
    "started"
}

/// Later dispatch: talk to the actor spawned by an earlier dispatch.
async fn ask() -> String {
    let (reply_tx, reply_rx) = oneshot::channel();
    ACTOR
        .get()
        .expect("actor started")
        .send(reply_tx)
        .expect("actor alive");
    format!("served={}", reply_rx.await.expect("actor replied"))
}

fn request(path: &str) -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"GET","path":"{path}","app":"{APP}"}}"#);
    let mut wire = u32::try_from(header.len())
        .expect("small header")
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(header.as_bytes());
    wire
}

fn sync_dispatch(path: &str) -> String {
    let wire = block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(request(path)))
        .expect("shared runtime available");
    String::from_utf8_lossy(&wire).into_owned()
}

#[test]
fn sync_dispatch_resources_and_detached_tasks_outlive_the_call() {
    const CHILD_ENV: &str = "VESPERA_JNI_SHARED_RUNTIME_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("current unit-test executable"),
        )
        .args([
            "--exact",
            "jni_impl::shared_runtime_tests::sync_dispatch_resources_and_detached_tasks_outlive_the_call",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run isolated shared-runtime child test");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated shared-runtime test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert!(set_runtime_worker_threads(2));
    // Bound a regression (hung actor) to a 504 instead of a hung test.
    assert!(vespera_inprocess::set_dispatch_timeout(
        Duration::from_secs(5)
    ));
    vespera_inprocess::register_app_named(APP, || {
        vespera_inprocess::Router::new()
            .route("/start", get(start))
            .route("/ask", get(ask))
    });

    // Dispatch 1 on this test thread creates the resources.
    let first = sync_dispatch("/start");
    assert!(first.contains(r#""status":200"#), "{first}");

    // The detached timer task completes with NO dispatch driving the runtime.
    let waited = Instant::now();
    while !DETACHED_RAN.load(Ordering::Acquire) {
        assert!(
            waited.elapsed() < Duration::from_secs(5),
            "detached tokio::spawn never progressed after the sync dispatch returned"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Dispatches 2 and 3 from two fresh OS threads reuse the actor.
    for expected in 1..=2u64 {
        let response = std::thread::spawn(|| sync_dispatch("/ask"))
            .join()
            .expect("dispatch thread");
        assert!(
            response.contains(r#""status":200"#)
                && response.ends_with(&format!("served={expected}")),
            "cross-thread reuse of a runtime-bound resource failed: {response}"
        );
    }
}
