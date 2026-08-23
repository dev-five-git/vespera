//! Ingress request-size cap ([`vespera_inprocess::max_request_bytes`]).
//!
//! Runs in its own test binary so the process-global `OnceLock` cap is
//! isolated from the other integration tests (which assume the default
//! unlimited behaviour).  Both tests pin the same cap so they are
//! order-independent under the parallel test runner.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;

use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::{
    DirectWriteResult, dispatch_from_bytes, dispatch_into_async_borrowed, dispatch_streaming_async,
    dispatch_streaming_with_header_async, set_max_request_bytes,
};

/// Small enough that a tiny valid header passes but a padded request
/// trips the cap.
const CAP: usize = 100;

fn ensure_cap() {
    // First-wins `OnceLock`; every test sets the same value so whichever
    // runs first, the effective cap is identical.
    let _ = set_max_request_bytes(CAP);
}

/// Parse the JSON header out of a `[u32 BE len | header JSON | body]`
/// wire response.
fn parse_header_json(resp: &[u8]) -> Value {
    assert!(resp.len() >= 4, "wire response too short");
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON")
}

fn dispatch(wire: Vec<u8>) -> Value {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    parse_header_json(&dispatch_from_bytes(wire, &runtime))
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
        .block_on(fut)
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

#[test]
fn oversized_borrowed_direct_request_returns_413() {
    ensure_cap();
    let wire = wire_with_body(200);
    assert!(wire.len() > CAP);
    let mut out = vec![0u8; 1024];

    let DirectWriteResult::Complete(n) = block_on(dispatch_into_async_borrowed(&wire, &mut out))
    else {
        panic!("413 error wire must fit");
    };

    let header = parse_header_json(&out[..n]);
    assert_eq!(header["status"].as_u64(), Some(413));
}

// ── Streaming-path ingress cap (INP-01) ──────────────────────────────
//
// Response streaming still buffers the full *request* in memory, so it
// must enforce the same cap as the buffered entry points — unlike
// bidirectional streaming, which pulls the request chunk-by-chunk and
// is intentionally exempt.

#[test]
fn oversized_streaming_request_returns_413() {
    ensure_cap();
    let wire = wire_with_body(200);
    assert!(wire.len() > CAP);

    let chunks = Cell::new(0usize);
    let header_bytes = block_on(dispatch_streaming_async(wire, |_chunk: &[u8]| {
        chunks.set(chunks.get() + 1);
        ControlFlow::Continue(())
    }));

    let header = parse_header_json(&header_bytes);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "response streaming buffers the full request, so an over-cap request must be 413"
    );
    assert_eq!(
        chunks.get(),
        0,
        "a capped request must never stream body chunks"
    );
}

#[test]
fn oversized_streaming_with_header_request_returns_413() {
    ensure_cap();
    let wire = wire_with_body(200);
    assert!(wire.len() > CAP);

    let header_seen: RefCell<Option<Vec<u8>>> = RefCell::new(None);
    let chunks = Cell::new(0usize);
    let _ = block_on(dispatch_streaming_with_header_async(
        wire,
        |header: &[u8]| *header_seen.borrow_mut() = Some(header.to_vec()),
        |_chunk: &[u8]| {
            chunks.set(chunks.get() + 1);
            ControlFlow::Continue(())
        },
    ));

    let header_bytes = header_seen
        .into_inner()
        .expect("the header callback must fire exactly once, even on the 413 cap path");
    let header = parse_header_json(&header_bytes);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "the 413 must be delivered through the header callback"
    );
    assert_eq!(
        chunks.get(),
        0,
        "a capped request must never stream body chunks"
    );
}
