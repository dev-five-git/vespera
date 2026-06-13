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
//! Run (Linux/macOS, nightly + `cargo install cargo-fuzz`):
//! ```text
//! cargo +nightly fuzz run wire_dispatch
//! ```
//!
//! The portable, deterministic counterpart that runs under plain
//! `cargo test` on every platform is
//! `crates/vespera_inprocess/tests/wire_robustness.rs`.

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use tokio::runtime::{Builder, Runtime};
use vespera_inprocess::dispatch_from_bytes;

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime")
    })
}

fuzz_target!(|data: &[u8]| {
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
