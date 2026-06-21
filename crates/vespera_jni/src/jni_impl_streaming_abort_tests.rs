use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{push_unless_header_failed, should_fire_fallback_header};

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

#[test]
fn fallback_header_fires_only_when_consumer_never_invoked() {
    // Panic unwound BEFORE the header callback was ever reached: the Java caller
    // has no header yet, so the one-shot 500 fallback MUST fire.
    assert!(should_fire_fallback_header(false, false));

    // Header callback already SUCCEEDED: re-firing would deliver the header
    // twice — forbidden by the "invoked exactly once on every code path" contract.
    assert!(!should_fire_fallback_header(true, false));

    // Header callback already THREW (it WAS invoked): a later panic must not
    // re-enter the (possibly broken / already-committed) consumer a second time.
    // This is the edge the prior `!header_sent`-only guard mishandled by
    // double-invoking the consumer.
    assert!(!should_fire_fallback_header(false, true));

    // Defensive: both flags set never co-occurs in practice, but must still not
    // re-fire.
    assert!(!should_fire_fallback_header(true, true));
}
