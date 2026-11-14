//! Helper functions + setup routines extracted from jni_impl.rs to keep that
//! file within the project's 1000-line source cap.  Pure code move — no logic
//! change.  All items are pub(super) (used only by the Java_... symbols in
//! [crate::jni_impl]).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use jni::objects::{Global, JObject};
use jni::strings::JNIStr;

use super::streaming_buffer::{
    PullPushBuffers, StreamingBufferRole, StreamingChunkBuffer, StreamingChunkBufferLease,
    checkout_pull_push_buffers, checkout_streaming_chunk_buffer,
};

/// Exception class thrown when a streaming dispatch aborts the Java
/// transport: `com.devfive.vespera.bridge.VesperaStreamAbortedException`
/// (an `IOException`), whose `panicked()` tells a Rust panic apart from a
/// body error / sink stop, so the proxy can answer a still-uncommitted
/// response with its own `500` (flagged panicked) instead of leaving the
/// status to the servlet container's error page.
pub const STREAM_ABORTED_CLASS: &JNIStr =
    jni::jni_str!("com/devfive/vespera/bridge/VesperaStreamAbortedException");

/// Why a streaming dispatch aborts the Java transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    /// The header consumer threw: the header was not (fully) applied.
    HeaderCallbackFailed,
    /// The response body errored or the chunk sink stopped after the header.
    BodyAborted,
    /// A Rust panic unwound out of the dispatch after the header was delivered.
    Panicked,
}

impl AbortReason {
    /// The reason for a dispatch whose header consumer threw (`header_failed`)
    /// or that panicked after the header was delivered (`panicked`).  A failed
    /// header callback wins: it is the first failure on that path.
    #[must_use]
    pub const fn from_flags(header_failed: bool, panicked: bool) -> Self {
        if header_failed {
            Self::HeaderCallbackFailed
        } else if panicked {
            Self::Panicked
        } else {
            Self::BodyAborted
        }
    }

    /// The exception message.  The [`Self::Panicked`] text is matched
    /// verbatim by `VesperaStreamAbortedException.PANIC_MESSAGE` on the Java
    /// side — keep the two in sync.
    #[must_use]
    pub fn message(self) -> &'static JNIStr {
        match self {
            Self::HeaderCallbackFailed => {
                jni::jni_str!("vespera: response header callback failed before body streaming")
            }
            Self::BodyAborted => {
                jni::jni_str!(
                    "vespera: response body stream aborted after the header was committed"
                )
            }
            Self::Panicked => {
                jni::jni_str!(
                    "vespera: panic in Rust engine after the response header was produced"
                )
            }
        }
    }
}

/// Throw the abort exception for `reason`.  Falls back to a plain
/// `java.io.IOException` (same message) when the typed class cannot be
/// resolved — a bridge JAR older than this cdylib, or a native image whose
/// JNI metadata does not register it — so the abort is never lost.
pub fn throw_streaming_abort(env: &mut jni::Env<'_>, reason: AbortReason) {
    let result = env.throw_new(STREAM_ABORTED_CLASS, reason.message());
    if !typed_throw_succeeded(&result, env.exception_check()) {
        env.exception_clear();
        let _ = env.throw_new(jni::jni_str!("java/io/IOException"), reason.message());
    }
}

/// Whether `Env::throw_new` left the requested exception pending.
///
/// jni 0.22 reports a SUCCESSFUL `ThrowNew` as `Err(Error::JavaException)`
/// (the "an exception is now pending" signal meant for `?` propagation) —
/// never `Ok`.  Treating any `Err` as a failure cleared the freshly thrown
/// typed exception and replaced it with the plain `IOException` fallback on
/// every call.  A lookup / constructor failure surfaces as another error
/// variant (or leaves nothing pending).
#[must_use]
pub const fn typed_throw_succeeded(result: &jni::errors::Result<()>, pending: bool) -> bool {
    pending && matches!(result, Err(jni::errors::Error::JavaException))
}

/// Liveness token for a request-pull closure handed to the bidirectional
/// dispatch.  The dispatch may DETACH a producer still parked in
/// `InputStream.read` (bounded join), so after it returns the caller asks
/// [`PullToken::producer_dropped`] before reusing the pooled pull array.
pub struct PullToken(Arc<()>);

impl PullToken {
    /// `true` once the wrapped pull closure has been dropped — the producer
    /// finished (or never started) and no Java array is in flight.
    #[must_use]
    pub fn producer_dropped(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }
}

/// A pull closure bundled with its liveness token.  Called through a
/// `&mut self` method so the wrapping closure captures the WHOLE struct
/// (edition-2021 disjoint capture would otherwise capture only `pull` and
/// drop the token early).
struct TrackedPull<P> {
    pull: P,
    _token: Arc<()>,
}

impl<P: FnMut() -> T, T> TrackedPull<P> {
    fn next(&mut self) -> T {
        (self.pull)()
    }
}

/// Wrap `pull` so it carries a [`PullToken`] reference for its lifetime.
pub fn track_pull<P, T>(pull: P) -> (impl FnMut() -> T + Send + 'static, PullToken)
where
    P: FnMut() -> T + Send + 'static,
{
    let token = Arc::new(());
    let mut tracked = TrackedPull {
        pull,
        _token: Arc::clone(&token),
    };
    (move || tracked.next(), PullToken(token))
}

pub fn push_unless_header_failed(
    header_failed: &AtomicBool,
    push: &mut impl FnMut(&[u8]) -> std::ops::ControlFlow<()>,
    chunk: &[u8],
) -> std::ops::ControlFlow<()> {
    if header_failed.load(Ordering::Acquire) {
        std::ops::ControlFlow::Break(())
    } else {
        push(chunk)
    }
}

/// Whether the panic-path fallback header (a one-shot `500`) should be delivered
/// after a Rust panic unwound out of a streaming-with-header dispatch.
///
/// It fires ONLY when the header consumer was never invoked: `header_sent`
/// records a SUCCESSFUL invocation and `header_failed` records one that THREW —
/// either flag means "already invoked once", so a later panic must NOT re-enter
/// the consumer.  Re-entry would break the documented "header consumer invoked
/// exactly once on every code path" contract and re-deliver to a consumer that
/// may already be in a failed / partially-committed state.  Only a panic that
/// unwound BEFORE the callback was ever reached (both flags false) earns the
/// fallback, so the Java caller is never left without a header.
///
/// (The prior inline guard tested `!header_sent` alone, which double-invoked the
/// consumer in the rare "callback threw, then the dispatch future panicked"
/// edge; this predicate closes that gap and is unit-tested in
/// `jni_impl_streaming_abort_tests.rs`.)
pub fn should_fire_fallback_header(header_sent: bool, header_failed: bool) -> bool {
    !header_sent && !header_failed
}

/// What the panic landing-pad of a streaming-with-header dispatch must do after
/// a Rust panic unwound out of the dispatch future, given whether the response
/// header was already delivered.
///
/// Mirror image of the SUCCESS branch's truncation handling: that branch throws
/// [`throw_streaming_abort`] when the body errors or the sink stops *after* the
/// header was committed (`failed_header || BodyError | SinkStopped`).  A panic
/// after a committed header is the SAME failure shape — the body is truncated
/// past a header the host already wrote — so it must abort the transport too,
/// not return cleanly over a short body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicHeaderAction {
    /// The header consumer was never invoked (`!header_sent && !header_failed`):
    /// deliver the one-shot `500` fallback header so the Java caller is never
    /// left without a header.
    FireFallbackHeader,
    /// The header was already committed (or its delivery threw): a panic now
    /// truncates the body past a committed header, so throw `IOException` to
    /// abort the response — symmetric with the body-error / sink-stop abort on
    /// the success branch.
    ThrowAbort,
}

/// Decide the panic-branch action from the two header flags.  Splitting it out
/// (like [`should_fire_fallback_header`], which it reuses) keeps the decision
/// unit-testable without a live JVM — see `jni_impl_streaming_abort_tests.rs`.
pub fn panic_post_header_action(header_sent: bool, header_failed: bool) -> PanicHeaderAction {
    if should_fire_fallback_header(header_sent, header_failed) {
        PanicHeaderAction::FireFallbackHeader
    } else {
        PanicHeaderAction::ThrowAbort
    }
}

/// Promoted refs + a checked-out chunk buffer for a response
/// streaming-with-header dispatch.  Aliased so the helper return type stays
/// under clippy's `type_complexity` cap.
pub type StreamHeaderSetup = (
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    jni::JavaVM,
    StreamingChunkBuffer,
    Option<StreamingChunkBufferLease>,
);

/// Promote the header-consumer + output-stream refs and check out the chunk
/// buffer for [`Java_..._dispatchStreamingWithHeader`].  Split out so the
/// dispatcher handles a (rare, OOM-driven) setup failure with a `let ... else`
/// that fires the header consumer exactly once, instead of a silently-ignored
/// `?` that would leave the Java caller hanging.
///
/// This is exactly [`setup_stream`] plus one promoted global ref, so it only
/// spells out what differs: BOTH null checks stay here and in this order, so
/// the `NullPtr("header_consumer")` / `NullPtr("output_stream")` error identity
/// and precedence are unchanged; the header ref is promoted next; everything
/// after that — including the buffer checkout that must remain the last
/// fallible step — is delegated so the lease-safety ordering invariant lives in
/// one place.
pub fn setup_stream_with_header(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<StreamHeaderSetup> {
    if header_consumer.is_null() {
        return Err(jni::errors::Error::NullPtr("header_consumer"));
    }
    if output_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("output_stream"));
    }
    let header_global: Global<JObject<'static>> = env.new_global_ref(header_consumer)?;
    let (stream_global, jvm, push_buf, push_buf_lease) = setup_stream(env, output_stream)?;
    Ok((header_global, stream_global, jvm, push_buf, push_buf_lease))
}

/// Promoted refs + both chunk buffers for a bidirectional
/// streaming-with-header dispatch.  Aliased to stay under `type_complexity`.
pub type FullStreamHeaderSetup = (
    Global<JObject<'static>>,
    Arc<Global<JObject<'static>>>,
    Global<JObject<'static>>,
    jni::JavaVM,
    PullPushBuffers,
);

/// Promote the refs and check out both chunk buffers for
/// [`Java_..._dispatchFullStreamingWithHeader`].  Split out both to keep that
/// dispatcher under the line cap and so a setup failure is handled with a
/// `let ... else` that fires the header consumer exactly once.
///
/// This is exactly [`setup_full_stream`] plus one promoted global ref, so it
/// only spells out what differs: all three null checks stay here and in this
/// order, so the `NullPtr("header_consumer")` / `NullPtr("input_stream")` /
/// `NullPtr("output_stream")` error identity and precedence are unchanged; the
/// header ref is promoted next; everything after that — including the buffer
/// checkout that must remain the last fallible step — is delegated so the
/// lease-safety ordering invariant lives in one place.
pub fn setup_full_stream_with_header(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    input_stream: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<FullStreamHeaderSetup> {
    if header_consumer.is_null() {
        return Err(jni::errors::Error::NullPtr("header_consumer"));
    }
    if input_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("input_stream"));
    }
    if output_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("output_stream"));
    }
    let header_global: Global<JObject<'static>> = env.new_global_ref(header_consumer)?;
    let (input_global, output_global, jvm, buffers) =
        setup_full_stream(env, input_stream, output_stream)?;
    Ok((header_global, input_global, output_global, jvm, buffers))
}

/// Promoted output-stream ref + a checked-out push chunk buffer for a
/// response-streaming dispatch (no header consumer).  Aliased to stay under
/// clippy's `type_complexity` cap.
pub type StreamSetup = (
    Global<JObject<'static>>,
    jni::JavaVM,
    StreamingChunkBuffer,
    Option<StreamingChunkBufferLease>,
);

/// Promote the output-stream ref and check out the push chunk buffer for
/// [`Java_..._dispatchStreaming`].  Split out so the dispatcher can handle a
/// (rare, OOM-driven) setup failure with a `let ... else` that returns a `500`
/// wire response, instead of a silently-ignored `?` that surfaced to Java as a
/// thrown exception + `null` return — breaking the "every failure is a valid
/// wire response" contract the other dispatch symbols uphold.  The buffer
/// checkout is last, so an earlier ref/VM failure never leaves a lease held.
pub fn setup_stream(
    env: &mut jni::Env<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<StreamSetup> {
    if output_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("output_stream"));
    }
    let stream_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    let (push_buf, push_buf_lease) =
        checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;
    Ok((stream_global, jvm, push_buf, push_buf_lease))
}

/// Promoted input/output refs and both chunk buffers for a bidirectional
/// streaming dispatch (no header consumer).  The input ref is `Arc`-wrapped so
/// pull and post-response close share one JVM global ref.
pub type FullStreamSetup = (
    Arc<Global<JObject<'static>>>,
    Global<JObject<'static>>,
    jni::JavaVM,
    PullPushBuffers,
);

/// Promote the refs and check out both chunk buffers for
/// [`Java_..._dispatchFullStreaming`].  Split out so a setup failure returns a
/// `500` wire response instead of a silently-ignored `?` (see [`setup_stream`]).
/// `checkout_pull_push_buffers` releases the pull lease for us if the push
/// checkout fails, and no lease is held if an earlier ref/VM promotion fails.
pub fn setup_full_stream(
    env: &mut jni::Env<'_>,
    input_stream: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<FullStreamSetup> {
    if input_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("input_stream"));
    }
    if output_stream.is_null() {
        return Err(jni::errors::Error::NullPtr("output_stream"));
    }
    let input_global: Arc<Global<JObject<'static>>> = Arc::new(env.new_global_ref(input_stream)?);
    let output_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    let buffers = checkout_pull_push_buffers(env)?;
    Ok((input_global, output_global, jvm, buffers))
}
