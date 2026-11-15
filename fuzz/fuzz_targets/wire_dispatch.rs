#![no_main]
//! Coverage-guided fuzz target for the binary wire trust boundary.
//!
//! libFuzzer feeds arbitrary bytes straight into
//! [`vespera_inprocess::dispatch_from_bytes`] and explores the parser;
//! the wire contract is asserted so any violation aborts and is
//! recorded as a reproducible crash:
//!
//! * it must **never panic** (no OOB / overflow / unwrap reachable from
//!   hostile input), and
//! * it must **always return a well-formed length-prefixed wire
//!   response** whose header is valid JSON carrying a numeric `status`.
//!
//! A default app and an `admin` app are registered before the first
//! dispatch, so a well-formed header gets past app resolution into request
//! building (method / URI / header validation, the header cap, the
//! `remote_addr` / `scheme` / `principal` extensions), the router and the
//! response path: the default app echoes what reached the handler from a
//! catch-all fallback and answers `/invalid` with a `422` envelope (the
//! `validation_errors` hoist); `admin` (selected by the `app` field) echoes
//! too. Without them every such input ended at the `404` "no app
//! registered" wire. (No panicking route: libFuzzer's panic hook aborts on
//! any panic, also one the dispatcher would catch.)
//!
//! Run (Linux/macOS, nightly + `cargo install cargo-fuzz`):
//! ```text
//! cargo +nightly fuzz run wire_dispatch
//! ```
//!
//! The portable, deterministic counterpart that runs under plain
//! `cargo test` on every platform is
//! `crates/vespera_inprocess/tests/wire_robustness.rs`.

use std::sync::{Once, OnceLock};

use axum::{
    Router,
    http::{HeaderMap, Method, StatusCode, Uri, header::CONTENT_TYPE},
    routing::{any, get},
};
use libfuzzer_sys::fuzz_target;
use tokio::runtime::{Builder, Runtime};
use vespera_inprocess::{Principal, dispatch_from_bytes, register_app, register_app_named};

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime")
    })
}

/// Everything request building handed the handler, as text.
async fn echo(
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    principal: Option<Principal>,
    body: axum::body::Bytes,
) -> String {
    format!(
        "{method} {uri} {} {principal:?} {}",
        headers.len(),
        body.len()
    )
}

async fn invalid() -> (StatusCode, [(axum::http::HeaderName, &'static str); 1], &'static str) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [(CONTENT_TYPE, "application/json")],
        r#"{"errors":[{"path":"name","message":"length is lower than 1"}]}"#,
    )
}

fn register_apps() {
    static APPS: Once = Once::new();
    APPS.call_once(|| {
        register_app(|| Router::new().route("/invalid", any(invalid)).fallback(echo));
        register_app_named("admin", || Router::new().route("/", get(echo)).fallback(echo));
    });
}

fuzz_target!(|data: &[u8]| {
    register_apps();
    let resp = dispatch_from_bytes(data.to_vec(), runtime());

    // Contract — a violation here is a crash libFuzzer records for replay.
    assert!(resp.len() >= 4, "response shorter than 4-byte length prefix");
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "header_len overflows response"
    );
    let header: serde_json::Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header valid JSON");
    assert!(
        header.get("status").and_then(serde_json::Value::as_u64).is_some(),
        "response header carries a numeric status"
    );
});
