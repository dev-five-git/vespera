//! Helper functions + setup routines extracted from jni_impl.rs to keep that
//! file within the project's 1000-line source cap.  Pure code move — no logic
//! change.  All items are pub(super) (used only by the Java_... symbols in
//! [crate::jni_impl]).

use std::sync::atomic::{AtomicBool, Ordering};

use jni::objects::{Global, JObject};

use super::streaming_buffer::{
    PullPushBuffers, StreamingBufferRole, StreamingChunkBuffer, StreamingChunkBufferLease,
    checkout_pull_push_buffers, checkout_streaming_chunk_buffer,
};

pub(super) fn throw_streaming_abort(env: &mut jni::Env<'_>, header_failed: bool) {
    if header_failed {
        let _ = env.throw_new(
            jni::jni_str!("java/io/IOException"),
            jni::jni_str!("vespera: response header callback failed before body streaming"),
        );
    } else {
        let _ = env.throw_new(
            jni::jni_str!("java/io/IOException"),
            jni::jni_str!("vespera: response body stream aborted after the header was committed"),
        );
    }
}

pub(super) fn push_unless_header_failed(
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

/// Promoted refs + a checked-out chunk buffer for a response
/// streaming-with-header dispatch.  Aliased so the helper return type stays
/// under clippy's `type_complexity` cap.
pub(super) type StreamHeaderSetup = (
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
pub(super) fn setup_stream_with_header(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<StreamHeaderSetup> {
    let header_global: Global<JObject<'static>> = env.new_global_ref(header_consumer)?;
    let stream_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    // One per-thread reusable Java chunk buffer for the whole stream.
    let (push_buf, push_buf_lease) =
        checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;
    Ok((header_global, stream_global, jvm, push_buf, push_buf_lease))
}

/// Promoted refs + both chunk buffers for a bidirectional
/// streaming-with-header dispatch.  Aliased to stay under `type_complexity`.
pub(super) type FullStreamHeaderSetup = (
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    jni::JavaVM,
    PullPushBuffers,
);

/// Promote the refs and check out both chunk buffers for
/// [`Java_..._dispatchFullStreamingWithHeader`].  Split out both to keep that
/// dispatcher under the line cap and so a setup failure is handled with a
/// `let ... else` that fires the header consumer exactly once.
pub(super) fn setup_full_stream_with_header(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    input_stream: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<FullStreamHeaderSetup> {
    let header_global: Global<JObject<'static>> = env.new_global_ref(header_consumer)?;
    let input_global: Global<JObject<'static>> = env.new_global_ref(input_stream)?;
    // Second InputStream ref for the post-response close (the first is moved
    // into the pull closure; `Global` is not `Clone`).
    let input_for_close: Global<JObject<'static>> = env.new_global_ref(input_stream)?;
    let output_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    // Pull and push run concurrently on different threads (the pull lease is
    // released for us if the push checkout fails).
    let buffers = checkout_pull_push_buffers(env)?;
    Ok((
        header_global,
        input_global,
        input_for_close,
        output_global,
        jvm,
        buffers,
    ))
}

/// Promoted output-stream ref + a checked-out push chunk buffer for a
/// response-streaming dispatch (no header consumer).  Aliased to stay under
/// clippy's `type_complexity` cap.
pub(super) type StreamSetup = (
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
pub(super) fn setup_stream(
    env: &mut jni::Env<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<StreamSetup> {
    let stream_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    let (push_buf, push_buf_lease) =
        checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push)?;
    Ok((stream_global, jvm, push_buf, push_buf_lease))
}

/// Promoted input/output refs (+ a second input ref for the post-response
/// close, since `Global` is not `Clone`) and both chunk buffers for a
/// bidirectional streaming dispatch (no header consumer).  Aliased to stay
/// under `type_complexity`.
pub(super) type FullStreamSetup = (
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    Global<JObject<'static>>,
    jni::JavaVM,
    PullPushBuffers,
);

/// Promote the refs and check out both chunk buffers for
/// [`Java_..._dispatchFullStreaming`].  Split out so a setup failure returns a
/// `500` wire response instead of a silently-ignored `?` (see [`setup_stream`]).
/// `checkout_pull_push_buffers` releases the pull lease for us if the push
/// checkout fails, and no lease is held if an earlier ref/VM promotion fails.
pub(super) fn setup_full_stream(
    env: &mut jni::Env<'_>,
    input_stream: &JObject<'_>,
    output_stream: &JObject<'_>,
) -> jni::errors::Result<FullStreamSetup> {
    let input_global: Global<JObject<'static>> = env.new_global_ref(input_stream)?;
    let input_for_close: Global<JObject<'static>> = env.new_global_ref(input_stream)?;
    let output_global: Global<JObject<'static>> = env.new_global_ref(output_stream)?;
    let jvm = env.get_java_vm()?;
    let buffers = checkout_pull_push_buffers(env)?;
    Ok((input_global, input_for_close, output_global, jvm, buffers))
}
