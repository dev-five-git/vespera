use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use super::support::{
    AbortReason, PanicHeaderAction, STREAM_ABORTED_CLASS, header_failure_reason,
    panic_post_header_action, push_unless_header_failed, should_fire_fallback_header,
    typed_throw_succeeded,
};

#[test]
fn typed_throw_is_kept_when_jni_reports_the_pending_exception() {
    use jni::errors::Error;
    // jni 0.22: a successful ThrowNew returns Err(JavaException) with the
    // new exception pending — the typed abort must be kept, not replaced.
    assert!(typed_throw_succeeded(&Err(Error::JavaException), true));
    // Nothing pending, or a lookup / constructor failure: fall back.
    assert!(!typed_throw_succeeded(&Err(Error::JavaException), false));
    assert!(!typed_throw_succeeded(&Err(Error::WrongObjectType), true));
    assert!(!typed_throw_succeeded(&Err(Error::ThrowFailed(-1)), true));
    assert!(!typed_throw_succeeded(&Ok(()), false));
}

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

#[test]
fn panic_post_header_action_aborts_once_header_is_committed() {
    // Panic BEFORE the header was ever delivered: the Java caller has no header,
    // so the one-shot 500 fallback must be delivered (never an abort, which
    // would leave the caller with neither a header nor a result).
    assert_eq!(
        panic_post_header_action(false, false),
        PanicHeaderAction::FireFallbackHeader
    );

    // Header already SUCCEEDED, then the dispatch future panicked mid-body: the
    // body is truncated past a committed header, so the transport must be
    // aborted — re-firing the consumer is forbidden (already invoked once).
    assert_eq!(
        panic_post_header_action(true, false),
        PanicHeaderAction::ThrowAbort
    );

    // Header delivery THREW (consumer already invoked, response already broken):
    // a later panic must abort rather than re-enter the consumer.
    assert_eq!(
        panic_post_header_action(false, true),
        PanicHeaderAction::ThrowAbort
    );

    // Defensive: both flags set never co-occurs, but must still abort, never
    // double-invoke the consumer.
    assert_eq!(
        panic_post_header_action(true, true),
        PanicHeaderAction::ThrowAbort
    );
}

#[test]
fn pull_token_reports_whether_the_pull_closure_is_still_alive() {
    let (mut pull, token) = super::support::track_pull(|| 7u8);
    assert!(
        !token.producer_dropped(),
        "closure alive -> array in flight"
    );
    assert_eq!(pull(), 7);
    let detached = std::thread::spawn(move || {
        let _ = pull();
    });
    detached.join().expect("producer thread");
    assert!(
        token.producer_dropped(),
        "closure dropped -> lease reusable"
    );
}

/// Round 31: a header callback that throws while the panic wire is delivered
/// (the panicked dispatch's fallback header) keeps the panic bit.
#[test]
fn a_failed_header_delivering_the_panic_wire_keeps_the_panic_bit() {
    assert_eq!(
        header_failure_reason(&super::panic_wire()),
        AbortReason::HeaderCallbackFailedAndPanicked
    );
    assert_eq!(
        header_failure_reason(&vespera_inprocess::error_wire(400, "bad")),
        AbortReason::HeaderCallbackFailed
    );
}

#[test]
fn stream_abort_reason_keeps_a_failed_header_callback_and_its_panic() {
    // A failed header callback keeps its message; a panic on the same
    // dispatch still reaches Java through the panic-carrying variant.
    assert_eq!(
        AbortReason::from_flags(true, true),
        AbortReason::HeaderCallbackFailedAndPanicked
    );
    assert_eq!(
        AbortReason::from_flags(true, false),
        AbortReason::HeaderCallbackFailed
    );
    assert_eq!(AbortReason::from_flags(false, true), AbortReason::Panicked);
    assert_eq!(
        AbortReason::from_flags(false, false),
        AbortReason::BodyAborted
    );
}

#[test]
fn stream_abort_messages_match_the_java_contract() {
    // `VesperaStreamAbortedException.PANIC_MESSAGE` matches the panic text
    // verbatim to set `panicked()`; the other two are distinct.
    assert_eq!(
        AbortReason::Panicked.message().to_str(),
        "vespera: panic in Rust engine after the response header was produced"
    );
    assert_eq!(
        AbortReason::BodyAborted.message().to_str(),
        "vespera: response body stream aborted after the header was committed"
    );
    assert_eq!(
        AbortReason::HeaderCallbackFailed.message().to_str(),
        "vespera: response header callback failed before body streaming"
    );
    // The panic bit travels in the message, so Java's `panicked()` (message
    // match) is true here too while the header-failed text is kept.
    assert_eq!(
        AbortReason::HeaderCallbackFailedAndPanicked
            .message()
            .to_str(),
        "vespera: response header callback failed before body streaming, and the Rust engine panicked"
    );
    assert_eq!(
        STREAM_ABORTED_CLASS.to_str(),
        "com/devfive/vespera/bridge/VesperaStreamAbortedException"
    );
}

/// A segment the Java stream refused reached nothing, so the dispatch may
/// still answer with its error header.
#[test]
fn a_refused_segment_is_not_counted_as_delivered() {
    use crate::streaming_closures::write_segments;

    let reached = AtomicBool::new(false);
    let result = write_segments(b"chunk", 4, &reached, |_| {
        Err(jni::errors::Error::JavaException)
    });
    assert!(result.is_err());
    assert!(
        !reached.load(Ordering::Acquire),
        "a refused segment reached nothing"
    );
}

/// Regression for the round-25 `record_delivery` bug: a chunk split into
/// several segments can fail after its first segment was accepted by Java.
/// Those bytes reached the sink, so the failure must abort the stream (after
/// delivery), not answer a clean error header, and the segments after the
/// failed write are not attempted.
#[test]
fn a_second_segment_failure_after_an_accepted_segment_aborts() {
    use crate::streaming_closures::write_segments;
    use vespera_inprocess::StreamOutcome;

    let reached = AtomicBool::new(false);
    let mut attempts = 0;
    let result = write_segments(b"0123456789", 4, &reached, |_| {
        attempts += 1;
        if attempts == 2 {
            Err(jni::errors::Error::JavaException)
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(
        attempts, 2,
        "segments after the failed write are not attempted"
    );
    let delivered = reached.load(Ordering::Acquire);
    assert!(delivered, "the first segment reached Java");
    assert!(super::stream_aborts(StreamOutcome::SinkStopped, delivered));
}

/// Only a truncation after a delivered chunk throws; a truncation before any
/// byte reached Java is answered by the header, and a complete stream never
/// throws.
#[test]
fn only_a_truncation_after_delivery_aborts() {
    use vespera_inprocess::StreamOutcome;

    assert!(!super::stream_aborts(StreamOutcome::Complete, true));
    assert!(!super::stream_aborts(StreamOutcome::SinkStopped, false));
    assert!(!super::stream_aborts(StreamOutcome::BodyError, false));
    assert!(super::stream_aborts(StreamOutcome::SinkStopped, true));
    assert!(super::stream_aborts(StreamOutcome::BodyError, true));
}
