//! Regression test for the `register_app_named` first-wins contract under
//! **concurrent** same-name registration.
//!
//! Before the registration write-lock, two (or more) threads racing to
//! register the same name could all pass the `contains_key` pre-check and
//! each invoke their `factory` — the loser's router was then silently
//! discarded by the first-wins insert.  That breaks the documented
//! "factory is NOT invoked for a duplicate name" contract and is observable
//! whenever a factory has side effects or is expensive.
//!
//! The fix serializes the *registration write path* with a lock (dispatch
//! reads stay lock-free), so the factory for a given name runs **at most
//! once**.  This test maximizes the race with a [`Barrier`] so every thread
//! hits `register_app_named` simultaneously, then asserts exactly one factory
//! invocation and that the first-wins router is dispatchable.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use axum::Router;
use axum::routing::get;
use serde_json::Value;
use vespera_inprocess::{dispatch_from_bytes, register_app_named};

/// Encode a wire request carrying an explicit `"app"` name (no body).
fn encode_wire_for_app(method: &str, path: &str, app: &str) -> Vec<u8> {
    let header = serde_json::json!({ "v": 1, "method": method, "path": path, "app": app });
    let header_bytes = serde_json::to_vec(&header).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire
}

/// Decode the wire response status from its length-prefixed JSON header.
fn decode_status(resp: &[u8]) -> u64 {
    assert!(resp.len() >= 4, "wire response too short ({})", resp.len());
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "wire header_len overflows response"
    );
    let header: Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header is valid JSON");
    header["status"].as_u64().expect("status is an integer")
}

#[test]
fn concurrent_same_name_register_invokes_factory_once() {
    const THREADS: usize = 16;
    let invocations = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(THREADS));

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let inv = Arc::clone(&invocations);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                // Release every thread at once to maximize the registration race.
                barrier.wait();
                register_app_named("race-app", move || {
                    inv.fetch_add(1, Ordering::SeqCst);
                    Router::new().route("/race", get(|| async { "ok" }))
                });
            })
        })
        .collect();
    for h in handles {
        h.join().expect("registration thread panicked");
    }

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "concurrent same-name register_app_named must invoke the factory exactly \
         once (first-wins); a count > 1 means racing registrations both ran their \
         factory before either inserted"
    );

    // The first-wins router must be dispatchable under its app name.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let resp = dispatch_from_bytes(encode_wire_for_app("GET", "/race", "race-app"), &runtime);
    assert_eq!(
        decode_status(&resp),
        200,
        "the first-wins race-app router must be reachable after concurrent registration"
    );
}
