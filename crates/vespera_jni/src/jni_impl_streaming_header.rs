use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use jni::EnvUnowned;
use jni::objects::{Global, JByteArray, JClass, JObject};

use crate::daemon_env::with_cached_daemon_env;
use crate::jni_impl::{
    guard_void_symbol, panic_wire, read_request_byte_array, runtime, runtime_unavailable_wire,
    streaming_buffer::{PullPushBuffers, mark_streaming_buffer_reusable},
    support::{
        FullStreamHeaderSetup, PanicHeaderAction, panic_post_header_action,
        push_unless_header_failed, setup_full_stream_with_header, setup_stream_with_header,
        throw_streaming_abort,
    },
};
use crate::streaming_closures::{
    call_header_consumer, call_header_consumer_local, close_input_stream, make_pull_closure,
    make_push_closure,
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
            let err = panic_wire();
            let _ = call_header_consumer_local(env, header_consumer, &err);
            flags.notified.store(true, Ordering::Release);
        }
        PanicHeaderAction::ThrowAbort => {
            throw_streaming_abort(env, failed);
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
    if failed_header
        || matches!(
            outcome,
            vespera_inprocess::StreamOutcome::BodyError
                | vespera_inprocess::StreamOutcome::SinkStopped
        )
    {
        throw_streaming_abort(env, failed_header);
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

fn notify_local_header(
    env: &mut jni::Env<'_>,
    header_consumer: &JObject<'_>,
    header_bytes: &[u8],
    flags: &StreamingFlags,
) {
    let _ = call_header_consumer_local(env, header_consumer, header_bytes);
    flags.notified.store(true, Ordering::Release);
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
            with_cached_daemon_env(&jvm, |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                call_header_consumer(env, &consumer, header_bytes)
            })
            .is_ok();
        flags.record_header_callback_result(delivered);
    }
}

/// Outer-panic header fallback shared by both `dispatch*WithHeader`
/// JNI symbols.  When `guard_void_symbol` intercepts a panic that escaped
/// the inner `with_env` scope AND the header consumer has not yet been
/// notified AND the caller passed a non-null consumer, deliver the
/// canonical `panic_wire()` bytes through it so the Java caller never
/// hangs waiting for a header that will never arrive.
///
/// Extracted verbatim from the two symbols' post-`guard_void_symbol`
/// tails so a future edit to the panic-recovery predicate (extra
/// atomic fence, logging, changed `panic_wire` shape) can never drift
/// between them.  `#[inline]` folds it back into each caller so codegen
/// matches the prior inline block byte-for-byte.
#[inline]
fn deliver_panic_header_if_needed<'local>(
    unowned_env: &mut EnvUnowned<'local>,
    header_consumer: &JObject<'local>,
    flags: &StreamingFlags,
    panicked: bool,
) {
    if panicked && !flags.notified.load(Ordering::Acquire) && !header_consumer.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            notify_local_header(env, header_consumer, &panic_wire(), flags);
            Ok(())
        });
    }
}

fn read_header_or_notify(
    env: &mut jni::Env<'_>,
    header_bytes: &JByteArray<'_>,
    header_consumer: &JObject<'_>,
    flags: &StreamingFlags,
) -> Option<Vec<u8>> {
    match read_request_byte_array(env, header_bytes) {
        Ok(buf) => Some(buf),
        Err(err) => {
            notify_local_header(env, header_consumer, &err, flags);
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
            notify_local_header(env, header_consumer, &panic_wire(), flags);
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
    let Some(header_input) =
        read_header_or_notify(env, args.header_bytes, args.header_consumer, args.flags)
    else {
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
        notify_local_header(
            env,
            args.header_consumer,
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
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut push = make_push_closure(push_jvm, push_global, push_buf);
        runtime.block_on(
            vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                header_input,
                make_pull_closure(pull_jvm, pull_global, pull_buf),
                move |chunk: &[u8]| {
                    push_unless_header_failed(&flags_for_push.failed, &mut push, chunk)
                },
                make_header_callback(header_jvm, header_for_cb, flags_for_cb),
                move || {
                    let _ = with_cached_daemon_env(&close_jvm, |env| {
                        close_input_stream(env, &input_for_close)
                    });
                },
            ),
        )
    }));
    match panic_result {
        Ok(outcome) => {
            mark_streaming_buffer_reusable(pull_buf_lease);
            mark_streaming_buffer_reusable(push_buf_lease);
            throw_if_stream_aborted(env, args.flags, outcome);
        }
        Err(_) => handle_header_dispatch_panic(env, args.header_consumer, args.flags),
    }
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
    let Some(input) =
        read_header_or_notify(env, args.request_bytes, args.header_consumer, args.flags)
    else {
        return;
    };

    let Ok((header_global, stream_global, jvm, push_buf, push_buf_lease)) =
        setup_stream_with_header(env, args.header_consumer, args.output_stream)
    else {
        notify_local_header(env, args.header_consumer, &panic_wire(), args.flags);
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
        notify_local_header(
            env,
            args.header_consumer,
            &runtime_unavailable_wire(),
            args.flags,
        );
        return;
    };

    let flags_for_cb = Arc::clone(args.flags);
    let flags_for_push = Arc::clone(args.flags);
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let jvm_for_cb = jvm.clone();
        let mut push = make_push_closure(jvm, stream_global, push_buf);
        runtime.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
            input,
            make_header_callback(jvm_for_cb, header_global, flags_for_cb),
            move |chunk: &[u8]| push_unless_header_failed(&flags_for_push.failed, &mut push, chunk),
        ))
    }));
    match panic_result {
        Ok(outcome) => {
            mark_streaming_buffer_reusable(push_buf_lease);
            throw_if_stream_aborted(env, args.flags, outcome);
        }
        Err(_) => handle_header_dispatch_panic(env, args.header_consumer, args.flags),
    }
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
    let panicked = guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
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
    });
    deliver_panic_header_if_needed(&mut unowned_env, &header_consumer, &flags, panicked);
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
    let panicked = guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
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
    });
    deliver_panic_header_if_needed(&mut unowned_env, &header_consumer, &flags, panicked);
}
