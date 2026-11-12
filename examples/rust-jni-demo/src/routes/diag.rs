//! Diagnostic routes for the JNI E2E suite: a response of a chosen size (the
//! buffered-response cap) and a deliberately slow handler (the Spring async
//! timeout). Each records how often it finished under a caller-chosen key, so
//! a test can prove the handler ran exactly once. Sizes and delays are clamped
//! so the routes stay harmless.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError},
    time::Duration,
};

use vespera::axum::extract::Path;

/// Largest body `/diag/blob` produces (16 MiB).
pub const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;
/// Longest delay `/diag/sleep` waits (10 s).
pub const MAX_SLEEP_MILLIS: u64 = 10_000;

static RUNS: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Mutex::default);

fn record(key: String) {
    *RUNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_default() += 1;
}

/// `bytes` bytes of `x` (at most [`MAX_BLOB_BYTES`]); counts a run of `key`.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/blob/{key}/{bytes}", tags = ["diag"])]
pub async fn blob(Path((key, bytes)): Path<(String, usize)>) -> Vec<u8> {
    record(key);
    vec![b'x'; bytes.min(MAX_BLOB_BYTES)]
}

/// Answers `slept` after `ms` milliseconds (at most [`MAX_SLEEP_MILLIS`]);
/// counts a run of `key` once the delay is over.
#[vespera::route(get, path = "/sleep/{key}/{ms}", tags = ["diag"])]
pub async fn sleep(Path((key, ms)): Path<(String, u64)>) -> &'static str {
    tokio::time::sleep(Duration::from_millis(ms.min(MAX_SLEEP_MILLIS))).await;
    record(key);
    "slept"
}

/// How many runs of `key` finished.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/runs/{key}", tags = ["diag"])]
pub async fn runs(Path(key): Path<String>) -> String {
    RUNS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .copied()
        .unwrap_or_default()
        .to_string()
}
