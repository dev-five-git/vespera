use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use super::push_unless_header_failed;

#[test]
fn push_gate_aborts_without_writing_when_header_delivery_failed() {
    // Given: the JNI header callback already failed before the first body chunk.
    let header_failed = AtomicBool::new(true);
    let mut wrote = false;

    // When: the response body pump tries to deliver a chunk.
    let outcome = push_unless_header_failed(
        &header_failed,
        &mut |_| {
            wrote = true;
            ControlFlow::Continue(())
        },
        b"body",
    );

    // Then: streaming aborts before any body byte reaches the sink.
    assert!(outcome.is_break());
    assert!(!wrote);
}

#[test]
fn push_gate_delegates_when_header_delivery_succeeded() {
    // Given: the header callback succeeded and body streaming may proceed.
    let header_failed = AtomicBool::new(false);
    let mut delivered = Vec::new();

    // When: the response body pump receives a chunk.
    let outcome = push_unless_header_failed(
        &header_failed,
        &mut |chunk| {
            delivered.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        b"body",
    );

    // Then: the underlying sink receives the bytes unchanged.
    assert!(outcome.is_continue());
    assert_eq!(delivered, b"body");

    header_failed.store(true, Ordering::SeqCst);
    let stopped =
        push_unless_header_failed(&header_failed, &mut |_| ControlFlow::Continue(()), b"x");
    assert!(stopped.is_break());
}
