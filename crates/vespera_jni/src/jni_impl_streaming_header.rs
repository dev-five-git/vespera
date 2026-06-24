use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, JObject};

use crate::daemon_env::with_cached_daemon_env;
use crate::jni_impl::{
    guard_void_symbol, panic_wire, read_request_byte_array, runtime,
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
    notified: Arc<AtomicBool>,
}

impl StreamingFlags {
    fn new(header_notified: Arc<AtomicBool>) -> Self {
        Self {
            sent: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            notified: header_notified,
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
    match panic_post_header_action(
        flags.sent.load(Ordering::Relaxed),
        flags.failed.load(Ordering::Acquire),
    ) {
        PanicHeaderAction::FireFallbackHeader => {
            let err = panic_wire();
            let _ = call_header_consumer_local(env, header_consumer, &err);
            flags.notified.store(true, Ordering::Release);
        }
        PanicHeaderAction::ThrowAbort => {
            throw_streaming_abort(env, flags.failed.load(Ordering::Acquire));
        }
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

fn runtime_unavailable_header() -> vespera_inprocess::StreamOutcome {
    vespera_inprocess::StreamOutcome::BodyError
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
        runtime().map_or_else(runtime_unavailable_header, |runtime| {
            runtime.block_on(
                vespera_inprocess::dispatch_bidirectional_streaming_with_header_closing(
                    header_input,
                    make_pull_closure(pull_jvm, pull_global, pull_buf),
                    move |chunk: &[u8]| {
                        push_unless_header_failed(&flags_for_push.failed, &mut push, chunk)
                    },
                    |header_bytes: &[u8]| {
                        let delivered = with_cached_daemon_env(
                            &header_jvm,
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                call_header_consumer(env, &header_for_cb, header_bytes)
                            },
                        )
                        .is_ok();
                        flags_for_cb.record_header_callback_result(delivered);
                    },
                    move || {
                        let _ = with_cached_daemon_env(&close_jvm, |env| {
                            close_input_stream(env, &input_for_close)
                        });
                    },
                ),
            )
        })
    }));
    match panic_result {
        Ok(outcome) => {
            mark_streaming_buffer_reusable(pull_buf_lease);
            mark_streaming_buffer_reusable(push_buf_lease);
            let failed_header = args.flags.failed_header();
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
    let header_notified = Arc::new(AtomicBool::new(false));
    let flags = Arc::new(StreamingFlags::new(Arc::clone(&header_notified)));
    let flags_body = Arc::clone(&flags);
    let panicked = guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            if reject_null_header_consumer(env, &header_consumer, &flags_body) {
                return Ok(());
            }
            let Some(input) =
                read_header_or_notify(env, &request_bytes, &header_consumer, &flags_body)
            else {
                return Ok(());
            };

            let Ok((header_global, stream_global, jvm, push_buf, push_buf_lease)) =
                setup_stream_with_header(env, &header_consumer, &output_stream)
            else {
                notify_local_header(env, &header_consumer, &panic_wire(), &flags_body);
                return Ok(());
            };

            let flags_for_cb = Arc::clone(&flags_body);
            let flags_for_push = Arc::clone(&flags_body);
            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let header_for_cb = header_global;
                let jvm_for_cb = jvm.clone();
                let mut push = make_push_closure(jvm, stream_global, push_buf);
                runtime().map_or_else(runtime_unavailable_header, |runtime| {
                    runtime.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
                        input,
                        |header_bytes: &[u8]| {
                            let delivered = with_cached_daemon_env(
                                &jvm_for_cb,
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    call_header_consumer(env, &header_for_cb, header_bytes)
                                },
                            )
                            .is_ok();
                            flags_for_cb.record_header_callback_result(delivered);
                        },
                        move |chunk: &[u8]| {
                            push_unless_header_failed(&flags_for_push.failed, &mut push, chunk)
                        },
                    ))
                })
            }));
            match panic_result {
                Ok(outcome) => {
                    mark_streaming_buffer_reusable(push_buf_lease);
                    let failed_header = flags_body.failed_header();
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
                Err(_) => handle_header_dispatch_panic(env, &header_consumer, &flags_body),
            }

            Ok(())
        });
    });
    if panicked && !header_notified.load(Ordering::Acquire) && !header_consumer.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            notify_local_header(env, &header_consumer, &panic_wire(), &flags);
            Ok(())
        });
    }
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
    let header_notified = Arc::new(AtomicBool::new(false));
    let flags = Arc::new(StreamingFlags::new(Arc::clone(&header_notified)));
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
    if panicked && !header_notified.load(Ordering::Acquire) && !header_consumer.is_null() {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            notify_local_header(env, &header_consumer, &panic_wire(), &flags);
            Ok(())
        });
    }
}
