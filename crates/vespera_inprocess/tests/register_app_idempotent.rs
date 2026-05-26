//! Integration test for the `register_app` first-wins semantics:
//! a second (or later) `register_app` call must be a no-op that
//! preserves the originally registered router, without invoking the
//! supplied factory closure a second time.
//!
//! Exercises the registered router via the binary wire API
//! ([`dispatch_from_bytes`]) — same code path the JNI bridge uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::routing::get;
use serde_json::Value;
use vespera_inprocess::{dispatch_from_bytes, register_app};

/// Encode a wire-format request for the given method + path with no
/// body and no headers.  Mirrors what the Java side will send.
fn encode_wire(method: &str, path: &str) -> Vec<u8> {
    let header = serde_json::json!({
        "v": 1,
        "method": method,
        "path": path,
    });
    let header_bytes = serde_json::to_vec(&header).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire
}

/// Decode a wire-format response: returns the header JSON as a value.
fn decode_wire_header(resp: &[u8]) -> Value {
    assert!(resp.len() >= 4, "wire response too short ({})", resp.len());
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "wire header_len {header_len} overflows response ({} bytes)",
        resp.len()
    );
    serde_json::from_slice(&resp[4..4 + header_len]).expect("response header is valid JSON")
}

#[test]
fn second_register_is_noop_first_wins() {
    let invocations = Arc::new(AtomicUsize::new(0));

    let inv = Arc::clone(&invocations);
    register_app(move || {
        inv.fetch_add(1, Ordering::SeqCst);
        Router::new().route("/from-first", get(|| async { "first" }))
    });

    let inv = Arc::clone(&invocations);
    register_app(move || {
        inv.fetch_add(100, Ordering::SeqCst);
        Router::new().route("/from-second", get(|| async { "second" }))
    });

    register_app(|| {
        unreachable!("third register_app call must be a no-op without invoking the factory");
    });

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "only the first register_app should have invoked its factory; \
         later calls must short-circuit before running the closure"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    // First registration's route must be reachable.
    let resp = dispatch_from_bytes(encode_wire("GET", "/from-first"), &runtime);
    let header = decode_wire_header(&resp);
    assert_eq!(
        header["status"].as_u64().expect("status is integer"),
        200,
        "first registration's route must still be reachable after the no-op second register_app: {header:#}"
    );

    // Second registration's route must NOT be reachable — the second
    // factory was never invoked so the router was never built.
    let resp = dispatch_from_bytes(encode_wire("GET", "/from-second"), &runtime);
    let header = decode_wire_header(&resp);
    assert_eq!(
        header["status"].as_u64().expect("status is integer"),
        404,
        "second registration was a no-op — its route must not exist on the registered router: {header:#}"
    );
}
