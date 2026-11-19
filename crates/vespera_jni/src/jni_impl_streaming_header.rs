use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use jni::EnvUnowned;
use jni::objects::{Global, JByteArray, JClass, JObject};

use crate::daemon_env::with_cached_daemon_env_no_frame;
use crate::jni_impl::{
    clear_pending_exception, guard_env_void_symbol, panic_wire, read_request_byte_array, runtime,
    runtime_unavailable_wire,
    streaming_buffer::{PullPushBuffers, mark_streaming_buffer_reusable, release_pull_lease},
    streaming_setup_failed_wire,
    support::{
        AbortReason, FullStreamHeaderSetup, PanicHeaderAction, panic_post_header_action,
        push_unless_header_failed, setup_full_stream_with_header, setup_stream_with_header,
        throw_streaming_abort, track_pull,
    },
};
use crate::streaming_closures::{
    call_header_consumer, call_header_consumer_local, close_input_stream, make_pull_closure,
    make_push_closure, write_output_local,
};

struct StreamingFlags {
    sent: AtomicBool,
    failed: AtomicBool,
    notified: AtomicBool,
}

impl StreamingFlags {
    fn new() -> Self {
        Self {
            sent: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            notified: AtomicBool::new(false),
        }
    }

    fn record_header_callback_result(&self, delivered: bool) {
        self.notified.store(true, Ordering::Release);
        if delivered {
            self.sent.store(true, Ordering::Relaxed);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn failed_header(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

fn handle_header_dispatch_panic(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    flags: &StreamingFlags,
) {
    // Cache once so `panic_post_header_action` and `throw_streaming_abort`
    // observe the exact same `failed` snapshot — the two loads happen
    // back-to-back on the panic path with no intervening writer, so a
    // second Acquire load could only ever see the same value.  Avoids a
    // duplicate atomic through opaque function boundaries.
    let failed = flags.failed.load(Ordering::Acquire);
    match panic_post_header_action(flags.sent.load(Ordering::Relaxed), failed) {
        PanicHeaderAction::FireFallbackHeader => {
            notify_local_wire(env, header_consumer, output_stream, &panic_wire(), flags);
        }
        PanicHeaderAction::ThrowAbort => {
            throw_streaming_abort(env, AbortReason::from_flags(failed, true));
        }
    }
}

/// Success-branch mirror of [`panic_post_header_action`]: decide whether a
/// completed dispatch must still abort the Java transport.
///
/// The two `dispatch*WithHeader` symbols share one truncation-reporting
/// contract (documented on [`PanicHeaderAction`]): a response whose header was
/// already committed but whose body did not drain cleanly — because the header
/// callback threw (`failed_header`), the body errored
/// ([`vespera_inprocess::StreamOutcome::BodyError`]), or the chunk sink stopped
/// early ([`vespera_inprocess::StreamOutcome::SinkStopped`]) — must throw
/// `IOException` rather than return normally over a short body.  Both symbols
/// previously carried a byte-for-byte identical copy of that predicate, so an
/// edit to one (an added outcome variant, a changed `failed_header` ordering)
/// could silently drift from the other and from the panic branch it mirrors;
/// the same drift hazard already motivated extracting
/// [`deliver_panic_header_if_needed`].
///
/// `failed_header()` is loaded exactly once and reused for both the predicate
/// and the thrown message selection, matching the single-snapshot discipline of
/// [`handle_header_dispatch_panic`].  `#[inline]` keeps codegen identical to
/// the prior inline blocks.
#[inline]
fn throw_if_stream_aborted(
    env: &mut jni::Env<'_>,
    flags: &StreamingFlags,
    outcome: vespera_inprocess::StreamOutcome,
) {
    let failed_header = flags.failed_header();
    if failed_header || outcome.is_truncated() {
        throw_streaming_abort(env, AbortReason::from_flags(failed_header, false));
    }
}

/// Post-dispatch tail shared by both `dispatch*WithHeader` JNI symbols.
///
/// Both symbols end their `catch_unwind` with a byte-for-byte identical
/// `match`, differing only in how many chunk-buffer leases they return
/// (one for response-only streaming, two for bidirectional).  The
/// asymmetry between the two arms is load-bearing and NOT an oversight:
///
/// * `Ok` — the streaming future returned, so no Java array can still be
///   in flight; the leases are marked reusable and the per-thread pool
///   keeps its cached buffers (see `jni_impl_streaming_buffer.rs`).
/// * `Err` — a panic unwound while the request producer may STILL be
///   parked in `InputStream.read` on a `spawn_blocking` thread, so
///   `release_buffers` must NOT run: dropping the lease un-released is
///   what makes `StreamingChunkBufferLease::Drop` discard the cached slot
///   instead of handing the possibly-in-flight array to the next
///   dispatch.
///
/// That subtlety previously lived in two places, which is the same drift
/// hazard that already motivated extracting [`throw_if_stream_aborted`],
/// [`make_header_callback`], [`deliver_panic_header_if_needed`] and
/// [`handle_header_dispatch_panic`] from these symbols.  `#[inline]`
/// keeps codegen identical to the prior inline `match` blocks.
#[inline]
fn finish_header_dispatch(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    flags: &StreamingFlags,
    panic_result: &std::thread::Result<vespera_inprocess::StreamOutcome>,
    release_buffers: impl FnOnce(),
) {
    // Borrowed, not consumed: `StreamOutcome` is `Copy` and the panic
    // payload is never inspected, so taking it by value would only move a
    // `Box<dyn Any>` in to drop it (which clippy's `needless_pass_by_value`
    // correctly flags).  The caller still drops the payload at the end of
    // its own scope, exactly as the prior inline `match` arms did.
    match panic_result {
        Ok(outcome) => {
            release_buffers();
            throw_if_stream_aborted(env, flags, *outcome);
        }
        // Deliberately does NOT call `release_buffers` — see the doc above.
        Err(_) => handle_header_dispatch_panic(env, header_consumer, output_stream, flags),
    }
}

fn reject_null_header_consumer(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    flags: &StreamingFlags,
) -> bool {
    if !header_consumer.is_null() {
        return false;
    }
    let _ = env.throw_new(
        jni::jni_str!("java/lang/IllegalArgumentException"),
        jni::jni_str!("headerConsumer must not be null"),
    );
    flags.notified.store(true, Ordering::Release);
    true
}

/// A null `OutputStream` / `InputStream` is a caller error: deliver the same
/// `400` wire header the non-header streaming symbols return for it
/// (`dispatchStreaming` / `dispatchFullStreaming`), not the `500 JNI
/// streaming setup failed` the null check inside the setup would produce.
fn reject_null_streams(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    streams: &[&JObject<'_>],
    message: &str,
    flags: &StreamingFlags,
) -> bool {
    if streams.iter().all(|stream| !stream.is_null()) {
        return false;
    }
    notify_local_wire(
        env,
        header_consumer,
        output_stream,
        &vespera_inprocess::error_wire(400, message),
        flags,
    );
    true
}

/// Split a locally built wire (`error_wire` / `panic_wire` / …) at its
/// header boundary: `(length-prefixed header, body)`.  The header-first
/// contract hands the consumer the header only; the body follows through
/// the output stream, as it does on the hot path (`pre_header_error` in
/// `vespera_inprocess::streaming`).  A wire too short for its prefix has no
/// body and passes through whole as the header.
fn split_local_wire(wire: &[u8]) -> (&[u8], &[u8]) {
    let header_len = wire.first_chunk::<4>().map_or(0, |len| {
        usize::try_from(u32::from_be_bytes(*len)).unwrap_or(usize::MAX)
    });
    wire.split_at(4usize.saturating_add(header_len).min(wire.len()))
}

/// Deliver a locally built wire (pre-dispatch error paths, panic fallback)
/// header-first, latching the outcome like the hot-path callback does: the
/// header goes to `header_consumer`, the body to `output_stream`.  A
/// consumer that threw must not let the native call return normally — Java
/// would finish as if the header had been applied — so the same
/// `VesperaStreamAbortedException` (`HeaderCallbackFailed`) as the main path
/// is thrown (the consumer's own exception was scrubbed by the cold-path
/// helper so the abort can be raised).  A body write that fails after the
/// header was applied is a truncated response, thrown as `BodyAborted`.
///
/// A null `output_stream` has nowhere to take the body, so only the header is
/// delivered; the callers that can receive a null stream reject it with a
/// `400` before any body is produced.
fn notify_local_wire(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    wire: &[u8],
    flags: &StreamingFlags,
) {
    let (header, body) = split_local_wire(wire);
    let delivered = call_header_consumer_local(env, header_consumer, header).is_ok();
    flags.record_header_callback_result(delivered);
    if !delivered {
        throw_streaming_abort(env, AbortReason::HeaderCallbackFailed);
        return;
    }
    if !body.is_empty()
        && !output_stream.is_null()
        && write_output_local(env, output_stream, body).is_err()
    {
        throw_streaming_abort(env, AbortReason::BodyAborted);
    }
}

/// JNI setup (global refs, the chunk `byte[]`) failed before the dispatch:
/// clear the pending Java exception and deliver the same
/// `500 JNI streaming setup failed` as the non-header streaming symbols —
/// not `panic_wire()`, which the bridge would log and count as a Rust panic.
fn notify_setup_failed(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    flags: &StreamingFlags,
) {
    clear_pending_exception(env);
    notify_local_wire(
        env,
        header_consumer,
        output_stream,
        &streaming_setup_failed_wire(),
        flags,
    );
}

/// Build the hot-path `on_header` callback shared by both `dispatch*WithHeader`
/// JNI symbols: deliver the wire header through the promoted `Consumer` on a
/// TLS-cached daemon-attached `JNIEnv`, then latch the outcome into the
/// `sent` / `failed` / `notified` flags via
/// [`StreamingFlags::record_header_callback_result`].
///
/// Both symbols previously inlined a byte-for-byte identical closure body,
/// differing only in the captured local names (`header_jvm` / `jvm_for_cb`,
/// `header_for_cb`). That duplication is the same drift hazard that already
/// motivated extracting [`throw_if_stream_aborted`] and
/// [`deliver_panic_header_if_needed`]: the `.is_ok()` → `record_header_callback_result`
/// pairing IS the "header consumer invoked exactly once on every code path"
/// contract, and an edit landing on only one copy would silently desynchronize
/// the two symbols' flag bookkeeping from each other and from the panic branch.
///
/// Callers construct it INSIDE their `catch_unwind` closure, at the point where
/// the `Global` consumer ref was moved before, so ownership and move semantics
/// are unchanged. `#[inline]` keeps codegen identical to the prior inline
/// closures.
#[inline]
fn make_header_callback(
    jvm: jni::JavaVM,
    consumer: Global<JObject<'static>>,
    flags: Arc<StreamingFlags>,
) -> impl FnMut(&[u8]) {
    move |header_bytes: &[u8]| {
        let delivered =
            // No outer frame: `call_header_consumer` opens its own (it frees
            // the header `byte[]`), so a second one was a redundant push/pop.
            with_cached_daemon_env_no_frame(&jvm, |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                call_header_consumer(env, &consumer, header_bytes)
            })
            .is_ok();
        flags.record_header_callback_result(delivered);
    }
}

/// Outer-panic header fallback shared by the `dispatch*WithHeader` and
/// `DirectStash.stream0` JNI symbols.  When `guard_env_void_symbol` reports a
/// panic that escaped the body (outside the dispatch's own `catch_unwind`,
/// e.g. in the setup) AND the header consumer has not yet been
/// notified AND the caller passed a non-null consumer, deliver the
/// canonical `panic_wire()` through [`notify_local_wire`]: its header to the
/// consumer, then its body to a non-null output stream (a null one gets the
/// header only), so the Java caller never hangs waiting for a header that
/// will never arrive.
///
/// Shared by the symbols' post-`guard_env_void_symbol` tails so a future
/// edit to the panic-recovery predicate (extra atomic fence, logging,
/// changed `panic_wire` shape) can never drift between them.  `#[inline]`
/// folds it back into each caller so codegen matches the prior inline block
/// byte-for-byte.
#[inline]
fn deliver_panic_header_if_needed<'local>(
    unowned_env: &mut EnvUnowned<'local>,
    header_consumer: &JObject<'local>,
    output_stream: &JObject<'local>,
    flags: &StreamingFlags,
    panicked: bool,
) {
    if panicked && !flags.notified.load(Ordering::Acquire) && !header_consumer.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            notify_local_wire(env, header_consumer, output_stream, &panic_wire(), flags);
            Ok(())
        });
    }
}

fn read_header_or_notify(
    env: &mut jni::Env<'_>,
    header_bytes: &JByteArray<'_>,
    header_consumer: &JObject<'_>,
    output_stream: &JObject<'_>,
    flags: &StreamingFlags,
) -> Option<Vec<u8>> {
    match read_request_byte_array(env, header_bytes) {
        Ok(buf) => Some(buf),
        Err(err) => {
            notify_local_wire(env, header_consumer, output_stream, &err, flags);
            None
        }
    }
}

fn setup_full_header_or_notify(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    input_stream: &JObject<'_>,
    output_stream: &JObject<'_>,
    flags: &StreamingFlags,
) -> Option<FullStreamHeaderSetup> {
    setup_full_stream_with_header(env, header_consumer, input_stream, output_stream).map_or_else(
        |_| {
            notify_setup_failed(env, header_consumer, output_stream, flags);
            None
        },
        Some,
    )
}

struct FullHeaderArgs<'a, 'local> {
    header_bytes: &'a JByteArray<'local>,
    header_consumer: &'a JObject<'local>,
    input_stream: &'a JObject<'local>,
    output_stream: &'a JObject<'local>,
    flags: &'a Arc<StreamingFlags>,
}

fn dispatch_full_streaming_with_header_body(env: &mut jni::Env<'_>, args: &FullHeaderArgs<'_, '_>) {
    if reject_null_header_consumer(env, args.header_consumer, args.flags) {
        return;
    }
    if reject_null_streams(
        env,
        args.header_consumer,
        args.output_stream,
        &[args.input_stream, args.output_stream],
        "inputStream and outputStream must not be null",
        args.flags,
    ) {
        return;
    }
    let Some(header_input) = read_header_or_notify(
        env,
        args.header_bytes,
        args.header_consumer,
        args.output_stream,
        args.flags,
    ) else {
        return;
    };
    let Some((header_global, input_global, output_global, jvm, buffers)) =
        setup_full_header_or_notify(
            env,
            args.header_consumer,
            args.input_stream,
            args.output_stream,
            args.flags,
        )
    else {
        return;
    };
    let PullPushBuffers {
        pull_buf,
        pull_buf_lease,
        push_buf,
        push_buf_lease,
    } = buffers;

    // Hoist the shared-runtime availability check OUT of `catch_unwind`: when
    // the OnceLock-cached Tokio runtime failed to initialize (OS resource
    // exhaustion at first dispatch), the documented "header consumer invoked
    // exactly once on every code path" contract REQUIRES delivering the wire
    // error THROUGH the header consumer — mirroring the read-failure and
    // setup-failure pre-dispatch shapes above.  The prior `.map_or_else` shape
    // inside the closure returned `StreamOutcome::BodyError`, which surfaced
    // as a misleading `IOException("...body stream aborted after the header
    // was committed")` for a body that was never produced and a header that
    // was never delivered — Java callers had no chance to handle the failure
    // through their `headerConsumer`.
    let Some(runtime) = runtime() else {
        mark_streaming_buffer_reusable(pull_buf_lease);
        mark_streaming_buffer_reusable(push_buf_lease);
        notify_local_wire(
            env,
            args.header_consumer,
            args.output_stream,
            &runtime_unavailable_wire(),
            args.flags,
        );
        return;
    };

    let pull_jvm = jvm.clone();
    let pull_global = Arc::clone(&input_global);
    let push_jvm = jvm.clone();
    let push_global = output_global;
    let close_jvm = jvm.clone();
    let input_for_close = input_global;
    let header_jvm = jvm;
    let header_for_cb = header_global;
    let flags_for_cb = Arc::clone(args.flags);
    let flags_for_push = Arc::clone(args.flags);
    // The pull closure carries a liveness token: a producer still parked in
    // `InputStream.read` after the bounded join is DETACHED, keeping the pull
    // array in flight, so the pull lease is only reused once it is dropped.
    let (pull, pull_token) = track_pull(make_pull_closure(pull_jvm, pull_global, pull_buf));
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut push = make_push_closure(
            push_jvm,
            push_global,
            push_buf,
            Arc::new(AtomicBool::new(false)),
        );
        runtime.block_on(
            vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                header_input,
                pull,
                move |chunk: &[u8]| {
                    push_unless_header_failed(&flags_for_push.failed, &mut push, chunk)
                },
                make_header_callback(header_jvm, header_for_cb, flags_for_cb),
                move || {
                    let _ = with_cached_daemon_env_no_frame(&close_jvm, |env| {
                        close_input_stream(env, &input_for_close)
                    });
                },
            ),
        )
    }));
    finish_header_dispatch(
        env,
        args.header_consumer,
        args.output_stream,
        args.flags,
        &panic_result,
        || {
            release_pull_lease(pull_buf_lease, pull_token.producer_dropped());
            mark_streaming_buffer_reusable(push_buf_lease);
        },
    );
}

struct StreamHeaderArgs<'a, 'local> {
    request_bytes: &'a JByteArray<'local>,
    header_consumer: &'a JObject<'local>,
    output_stream: &'a JObject<'local>,
    flags: &'a Arc<StreamingFlags>,
}

fn dispatch_streaming_with_header_body(env: &mut jni::Env<'_>, args: &StreamHeaderArgs<'_, '_>) {
    if reject_null_header_consumer(env, args.header_consumer, args.flags) {
        return;
    }
    if reject_null_streams(
        env,
        args.header_consumer,
        args.output_stream,
        &[args.output_stream],
        "outputStream must not be null",
        args.flags,
    ) {
        return;
    }
    let Some(input) = read_header_or_notify(
        env,
        args.request_bytes,
        args.header_consumer,
        args.output_stream,
        args.flags,
    ) else {
        return;
    };

    let Ok((header_global, stream_global, jvm, push_buf, push_buf_lease)) =
        setup_stream_with_header(env, args.header_consumer, args.output_stream)
    else {
        notify_setup_failed(env, args.header_consumer, args.output_stream, args.flags);
        return;
    };

    // Hoist the shared-runtime availability check OUT of `catch_unwind`:
    // when the OnceLock-cached Tokio runtime failed to initialize, the
    // documented "header consumer invoked exactly once on every code
    // path" contract REQUIRES delivering the wire error THROUGH the
    // header consumer (see the matching comment in
    // `dispatch_full_streaming_with_header_body`).  The prior
    // `.map_or_else` shape returned `StreamOutcome::BodyError`, which
    // surfaced as a misleading `IOException("...body stream aborted
    // after the header was committed")` for a body that was never
    // produced and a header that was never delivered.
    let Some(runtime) = runtime() else {
        mark_streaming_buffer_reusable(push_buf_lease);
        notify_local_wire(
            env,
            args.header_consumer,
            args.output_stream,
            &runtime_unavailable_wire(),
            args.flags,
        );
        return;
    };

    let flags_for_cb = Arc::clone(args.flags);
    let flags_for_push = Arc::clone(args.flags);
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let jvm_for_cb = jvm.clone();
        let mut push = make_push_closure(
            jvm,
            stream_global,
            push_buf,
            Arc::new(AtomicBool::new(false)),
        );
        runtime.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
            input,
            make_header_callback(jvm_for_cb, header_global, flags_for_cb),
            move |chunk: &[u8]| push_unless_header_failed(&flags_for_push.failed, &mut push, chunk),
        ))
    }));
    finish_header_dispatch(
        env,
        args.header_consumer,
        args.output_stream,
        args.flags,
        &panic_result,
        || {
            mark_streaming_buffer_reusable(push_buf_lease);
        },
    );
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreamingWithHeader<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    request_bytes: JByteArray<'local>,
    header_consumer: JObject<'local>,
    output_stream: JObject<'local>,
) {
    let flags = Arc::new(StreamingFlags::new());
    let flags_body = Arc::clone(&flags);
    let panicked = guard_env_void_symbol(&mut unowned_env, |env| {
        dispatch_streaming_with_header_body(
            env,
            &StreamHeaderArgs {
                request_bytes: &request_bytes,
                header_consumer: &header_consumer,
                output_stream: &output_stream,
                flags: &flags_body,
            },
        );
        Ok(())
    });
    deliver_panic_header_if_needed(
        &mut unowned_env,
        &header_consumer,
        &output_stream,
        &flags,
        panicked,
    );
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchFullStreamingWithHeader<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    header_bytes_in: JByteArray<'local>,
    header_consumer: JObject<'local>,
    input_stream: JObject<'local>,
    output_stream: JObject<'local>,
) {
    let flags = Arc::new(StreamingFlags::new());
    let flags_body = Arc::clone(&flags);
    let panicked = guard_env_void_symbol(&mut unowned_env, |env| {
        dispatch_full_streaming_with_header_body(
            env,
            &FullHeaderArgs {
                header_bytes: &header_bytes_in,
                header_consumer: &header_consumer,
                input_stream: &input_stream,
                output_stream: &output_stream,
                flags: &flags_body,
            },
        );
        Ok(())
    });
    deliver_panic_header_if_needed(
        &mut unowned_env,
        &header_consumer,
        &output_stream,
        &flags,
        panicked,
    );
}

/// Wire `500` body when a DIRECT overflow stash token is no longer known.
const STASH_EXPIRED_MSG: &str = "vespera DIRECT overflow stash entry expired or already redeemed";

struct StashStreamArgs<'a, 'local> {
    token: jni::sys::jlong,
    header_consumer: &'a JObject<'local>,
    output_stream: &'a JObject<'local>,
    flags: &'a Arc<StreamingFlags>,
}

/// Body of `DirectStash.stream0`: deliver a stashed DIRECT overflow through
/// the same header-first streaming contract as
/// `dispatchStreamingWithHeader` (header consumer exactly once, flush hints,
/// `IOException` on a post-header truncation) — the handler is NOT re-run.
fn stream_stashed_body(env: &mut jni::Env<'_>, args: &StashStreamArgs<'_, '_>) {
    if reject_null_header_consumer(env, args.header_consumer, args.flags) {
        return;
    }
    if reject_null_streams(
        env,
        args.header_consumer,
        args.output_stream,
        &[args.output_stream],
        "outputStream must not be null",
        args.flags,
    ) {
        return;
    }
    let Some(pending) = crate::jni_impl::direct_stash::take_stashed(args.token) else {
        // A shutdown cleared the stash after the overflow was stashed: the
        // retryable `503` of every other request, not "expired".
        let wire = if vespera_inprocess::runtime::runtime_shut_down() {
            runtime_unavailable_wire()
        } else {
            vespera_inprocess::error_wire(500, STASH_EXPIRED_MSG)
        };
        notify_local_wire(
            env,
            args.header_consumer,
            args.output_stream,
            &wire,
            args.flags,
        );
        return;
    };
    let Ok((header_global, stream_global, jvm, push_buf, push_buf_lease)) =
        setup_stream_with_header(env, args.header_consumer, args.output_stream)
    else {
        notify_setup_failed(env, args.header_consumer, args.output_stream, args.flags);
        return;
    };
    let Some(runtime) = runtime() else {
        mark_streaming_buffer_reusable(push_buf_lease);
        notify_local_wire(
            env,
            args.header_consumer,
            args.output_stream,
            &runtime_unavailable_wire(),
            args.flags,
        );
        return;
    };
    let flags_for_cb = Arc::clone(args.flags);
    let flags_for_push = Arc::clone(args.flags);
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let jvm_for_cb = jvm.clone();
        let mut push = make_push_closure(
            jvm,
            stream_global,
            push_buf,
            Arc::new(AtomicBool::new(false)),
        );
        runtime.block_on(pending.stream(
            make_header_callback(jvm_for_cb, header_global, flags_for_cb),
            move |chunk: &[u8]| push_unless_header_failed(&flags_for_push.failed, &mut push, chunk),
        ))
    }));
    finish_header_dispatch(
        env,
        args.header_consumer,
        args.output_stream,
        args.flags,
        &panic_result,
        || {
            mark_streaming_buffer_reusable(push_buf_lease);
        },
    );
}

/// `com.devfive.vespera.bridge.DirectStash.stream0(long, Consumer<byte[]>, OutputStream) -> void`
///
/// Stream the stashed DIRECT overflow `token` header-first (see
/// [`stream_stashed_body`]); an unknown token delivers a `500` header, or a
/// `503` header once the runtime has been shut down (the shutdown clears the
/// stash, so a token it dropped reads as unknown).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_DirectStash_stream0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    token: jni::sys::jlong,
    header_consumer: JObject<'local>,
    output_stream: JObject<'local>,
) {
    let flags = Arc::new(StreamingFlags::new());
    let flags_body = Arc::clone(&flags);
    let panicked = guard_env_void_symbol(&mut unowned_env, |env| {
        stream_stashed_body(
            env,
            &StashStreamArgs {
                token,
                header_consumer: &header_consumer,
                output_stream: &output_stream,
                flags: &flags_body,
            },
        );
        Ok(())
    });
    deliver_panic_header_if_needed(
        &mut unowned_env,
        &header_consumer,
        &output_stream,
        &flags,
        panicked,
    );
}

#[cfg(test)]
mod tests {
    use super::split_local_wire;

    #[test]
    fn local_wire_splits_at_the_header_boundary() {
        let wire = vespera_inprocess::error_wire(400, "invalid input byte array (null)");
        let (header, body) = split_local_wire(&wire);
        assert_eq!(body, b"invalid input byte array (null)");
        assert_eq!(header.len() + body.len(), wire.len());
        // The header is the length-prefixed JSON only: it ends where the body starts.
        assert_eq!(&wire[..header.len()], header);
    }

    #[test]
    fn short_or_bodyless_wire_is_all_header() {
        let (header, body) = split_local_wire(&[0, 0]);
        assert_eq!(header, &[0, 0]);
        assert_eq!(body, &[] as &[u8]);

        let mut bodyless = vec![0, 0, 0, 2];
        bodyless.extend_from_slice(b"{}");
        let (header, body) = split_local_wire(&bodyless);
        assert_eq!(header, bodyless.as_slice());
        assert_eq!(body, &[] as &[u8]);
    }
}
