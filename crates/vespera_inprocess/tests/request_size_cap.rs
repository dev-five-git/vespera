//! Ingress request-size cap ([`vespera_inprocess::max_request_bytes`]).
//!
//! Runs in its own test binary so the process-global `OnceLock` cap is
//! isolated from the other integration tests (which assume the default
//! unlimited behaviour).  Both tests pin the same cap so they are
//! order-independent under the parallel test runner.

use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::{dispatch_from_bytes, set_max_request_bytes};

/// Small enough that a tiny valid header passes but a padded request
/// trips the cap.
const CAP: usize = 100;

fn ensure_cap() {
    // First-wins `OnceLock`; every test sets the same value so whichever
    // runs first, the effective cap is identical.
    let _ = set_max_request_bytes(CAP);
}

fn dispatch(wire: Vec<u8>) -> Value {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let resp = dispatch_from_bytes(wire, &runtime);
    assert!(resp.len() >= 4, "wire response too short");
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON")
}

fn wire_with_body(body_len: usize) -> Vec<u8> {
    let header = br#"{"v":1,"method":"GET","path":"/ping"}"#;
    let mut wire = Vec::new();
    wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(header);
    wire.extend(std::iter::repeat_n(b'x', body_len));
    wire
}

#[test]
fn oversized_request_returns_413() {
    ensure_cap();
    let wire = wire_with_body(200); // total well over CAP
    assert!(wire.len() > CAP);
    let header = dispatch(wire);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "a request over the cap must be rejected with 413 before allocation"
    );
}

#[test]
fn within_limit_request_is_not_capped() {
    ensure_cap();
    let wire = wire_with_body(0); // small header-only request, under CAP
    assert!(wire.len() <= CAP);
    let header = dispatch(wire);
    // No app is registered in this test binary, so a within-limit request
    // falls through to the normal 404 (unknown app) — crucially NOT 413.
    assert_ne!(
        header["status"].as_u64(),
        Some(413),
        "a request within the cap must not be rejected as oversized"
    );
}
