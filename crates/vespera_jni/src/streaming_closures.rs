//! Streaming closure factories and Java-side callback helpers.
//!
//! These helpers are shared by every `dispatch*Streaming*` JNI
//! entry symbol in [`crate::jni_impl`].  They are split out into
//! a sibling module so:
//!
//! * `jni_impl.rs` stays inside the repo's 1000-line file cap
//!   while keeping every `Java_..._dispatch*` symbol together.
//! * The `JMethodID` cache for the per-chunk `InputStream.read` /
//!   `OutputStream.write` calls and the repeated callback helpers
//!   (`Consumer.accept` / `CompletableFuture.complete`) stays beside
//!   the only call sites that rely on it.
//!
//! All items are `pub(crate)` — never re-exported from the crate
//! root — so the JNI ABI surface (the `Java_...` symbols) lives
//! exclusively in [`crate::jni_impl`].

use std::sync::OnceLock;

use jni::ids::JMethodID;
use jni::objects::{JClass, JObject};
use jni::refs::Global;
use jni::signature::{MethodSignature, Primitive, ReturnType};
use jni::strings::JNIStr;
use jni::sys::{jint, jvalue};
use jni::{JValue, JValueOwned, jni_sig, jni_str};

use crate::daemon_env::with_cached_daemon_env;
use crate::jni_impl::streaming_chunk_size;

struct CachedMethod {
    _class: Global<JClass<'static>>,
    method_id: JMethodID,
}

impl CachedMethod {
    fn resolve<'sig, 'sig_args, C, N, S>(
        env: &mut jni::Env<'_>,
        class_name: C,
        method_name: N,
        method_sig: S,
    ) -> jni::errors::Result<Self>
    where
        C: AsRef<JNIStr>,
        N: AsRef<JNIStr>,
        S: AsRef<MethodSignature<'sig, 'sig_args>>,
    {
        let class = env.find_class(class_name)?;
        let method_id = env.get_method_id(&class, method_name, method_sig)?;
        let class = env.new_global_ref(&class)?;
        Ok(Self {
            _class: class,
            method_id,
        })
    }

    fn method_id(&self) -> JMethodID {
        // `_class` pins the Java class for as long as this method ID is cached:
        // JNI method IDs can be invalidated if their class unloads.
        self.method_id
    }
}

struct MethodCache {
    input_stream_read: CachedMethod,
    output_stream_write: CachedMethod,
    consumer_accept: CachedMethod,
    future_complete: CachedMethod,
}

impl MethodCache {
    fn resolve(env: &mut jni::Env<'_>) -> jni::errors::Result<Self> {
        env.with_local_frame::<_, _, jni::errors::Error>(16, |env| {
            Ok(Self {
                input_stream_read: CachedMethod::resolve(
                    env,
                    jni_str!("java/io/InputStream"),
                    jni_str!("read"),
                    jni_sig!("([B)I"),
                )?,
                output_stream_write: CachedMethod::resolve(
                    env,
                    jni_str!("java/io/OutputStream"),
                    jni_str!("write"),
                    jni_sig!("([BII)V"),
                )?,
                consumer_accept: CachedMethod::resolve(
                    env,
                    jni_str!("java/util/function/Consumer"),
                    jni_str!("accept"),
                    jni_sig!("(Ljava/lang/Object;)V"),
                )?,
                future_complete: CachedMethod::resolve(
                    env,
                    jni_str!("java/util/concurrent/CompletableFuture"),
                    jni_str!("complete"),
                    jni_sig!("(Ljava/lang/Object;)Z"),
                )?,
            })
        })
    }
}

static METHOD_CACHE: OnceLock<MethodCache> = OnceLock::new();

fn method_cache(env: &mut jni::Env<'_>) -> Option<&'static MethodCache> {
    if let Some(cache) = METHOD_CACHE.get() {
        return Some(cache);
    }

    let Ok(cache) = MethodCache::resolve(env) else {
        // Cache init is best-effort.  If class lookup, method lookup,
        // or global-ref promotion fails, clear only that init-time
        // exception and run the exact old string-based call path below.
        if env.exception_check() {
            env.exception_clear();
        }
        return None;
    };

    let _ = METHOD_CACHE.set(cache);
    METHOD_CACHE.get()
}

fn can_call_unchecked(obj: &Global<JObject<'static>>) -> bool {
    !obj.as_ref().as_raw().is_null()
}

fn call_cached_method<'local>(
    env: &mut jni::Env<'local>,
    obj: &Global<JObject<'static>>,
    method: &CachedMethod,
    ret_ty: ReturnType,
    args: &[jvalue],
) -> jni::errors::Result<JValueOwned<'local>> {
    // SAFETY: every `CachedMethod` is resolved by the JVM from a
    // bootstrap `java.*` class using the exact name/signature strings
    // previously passed to `Env::call_method`, and its `Global<JClass>`
    // pins that class for the process lifetime.  Each caller builds raw
    // `jvalue` arguments from the same `JValue` list as the former
    // checked call and passes the matching `ReturnType`; null receivers
    // are routed to the checked fallback before reaching this helper.
    unsafe { env.call_method_unchecked(obj, method.method_id(), ret_ty, args) }
}

fn call_input_stream_read(
    env: &mut jni::Env<'_>,
    stream: &Global<JObject<'static>>,
    buf: &Global<jni::objects::JByteArray<'static>>,
) -> jni::errors::Result<jint> {
    if can_call_unchecked(stream)
        && let Some(cache) = method_cache(env)
    {
        let args: [jvalue; 1] = [JValue::Object(buf.as_ref()).as_jni()];
        return call_cached_method(
            env,
            stream,
            &cache.input_stream_read,
            ReturnType::Primitive(Primitive::Int),
            &args,
        )?
        .i();
    }

    env.call_method(
        stream,
        jni_str!("read"),
        jni_sig!("([B)I"),
        &[JValue::Object(buf.as_ref())],
    )?
    .i()
}

fn call_output_stream_write(
    env: &mut jni::Env<'_>,
    stream: &Global<JObject<'static>>,
    buf: &Global<jni::objects::JByteArray<'static>>,
    len: jint,
) -> jni::errors::Result<()> {
    if can_call_unchecked(stream)
        && let Some(cache) = method_cache(env)
    {
        let args: [jvalue; 3] = [
            JValue::Object(buf.as_ref()).as_jni(),
            JValue::Int(0).as_jni(),
            JValue::Int(len).as_jni(),
        ];
        call_cached_method(
            env,
            stream,
            &cache.output_stream_write,
            ReturnType::Primitive(Primitive::Void),
            &args,
        )?;
        return Ok(());
    }

    env.call_method(
        stream,
        jni_str!("write"),
        jni_sig!("([BII)V"),
        &[
            JValue::Object(buf.as_ref()),
            JValue::Int(0),
            JValue::Int(len),
        ],
    )?;
    Ok(())
}

fn call_consumer_accept(
    env: &mut jni::Env<'_>,
    consumer: &Global<JObject<'static>>,
    arg: &JObject<'_>,
) -> jni::errors::Result<()> {
    if can_call_unchecked(consumer)
        && let Some(cache) = method_cache(env)
    {
        let args: [jvalue; 1] = [JValue::Object(arg).as_jni()];
        call_cached_method(
            env,
            consumer,
            &cache.consumer_accept,
            ReturnType::Primitive(Primitive::Void),
            &args,
        )?;
        return Ok(());
    }

    env.call_method(
        consumer,
        jni_str!("accept"),
        jni_sig!("(Ljava/lang/Object;)V"),
        &[JValue::Object(arg)],
    )?;
    Ok(())
}

fn call_future_complete(
    env: &mut jni::Env<'_>,
    future: &Global<JObject<'static>>,
    arg: &JObject<'_>,
) -> jni::errors::Result<()> {
    if can_call_unchecked(future)
        && let Some(cache) = method_cache(env)
    {
        let args: [jvalue; 1] = [JValue::Object(arg).as_jni()];
        call_cached_method(
            env,
            future,
            &cache.future_complete,
            ReturnType::Primitive(Primitive::Boolean),
            &args,
        )?;
        return Ok(());
    }

    env.call_method(
        future,
        jni_str!("complete"),
        jni_sig!("(Ljava/lang/Object;)Z"),
        &[JValue::Object(arg)],
    )?;
    Ok(())
}

/// Build the request-body pull closure shared by the two
/// full-streaming JNI entry points.
///
/// The Java-side chunk buffer (`buf`) is allocated **once** by the
/// caller and promoted to a global ref — reused across every
/// chunk instead of `new_byte_array` per chunk.  Bytes are copied
/// out via `get_byte_array_region`, which copies **only the `n`
/// bytes actually read** (the previous `convert_byte_array`
/// approach copied the full 16 KiB buffer regardless and then
/// truncated).
pub fn make_pull_closure(
    jvm: jni::JavaVM,
    stream: Global<JObject<'static>>,
    buf: Global<jni::objects::JByteArray<'static>>,
) -> impl FnMut() -> vespera_inprocess::RequestChunk + Send + 'static {
    use vespera_inprocess::RequestChunk;
    let chunk_size = streaming_chunk_size();
    move || -> RequestChunk {
        // Daemon-attach this (Tokio `spawn_blocking`) thread once,
        // cached in TLS, instead of attach+detach per chunk; the helper
        // also wraps the body in a fresh local-reference frame.
        let result: jni::errors::Result<RequestChunk> = with_cached_daemon_env(&jvm, |env| {
            let n = call_input_stream_read(env, &stream, &buf)?;
            if env.exception_check() {
                env.exception_clear();
            }
            // InputStream.read(byte[]) contract (mirrored in the
            // VesperaBridge javadoc): -1 = EOF, 0 = empty read that
            // MUST be retried.  The inprocess producer skips empty
            // chunks and keeps pulling, so report `0` as an empty
            // chunk rather than end-of-stream.
            if n < 0 {
                return Ok(RequestChunk::End);
            }
            if n == 0 {
                return Ok(RequestChunk::Data(Vec::new()));
            }
            let n = usize::try_from(n).expect("positive read length fits usize");
            let n = n.min(chunk_size);
            let mut data = vec![0u8; n];
            // SAFETY: `u8` and `i8` (JNI's `jbyte`) have
            // identical size/alignment; this views the
            // freshly allocated buffer as the signed slice
            // `get_byte_array_region` expects.
            let data_i8 =
                unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr().cast::<i8>(), n) };
            let arr: &jni::objects::JByteArray<'_> = buf.as_ref();
            arr.get_region(env, 0, data_i8)?;
            Ok(RequestChunk::Data(data))
        });
        // A JNI failure here — most importantly a `InputStream.read`
        // that threw (jni-rs surfaces a pending Java exception as
        // `Err`) — aborts the request body via `RequestChunk::Error`
        // instead of being silently mistaken for a clean EOF, so a
        // truncated upload is rejected rather than accepted as complete.
        result.unwrap_or(RequestChunk::Error)
    }
}
/// Build the response-body push closure shared by all four
/// streaming JNI entry points.
///
/// The Java-side buffer (`buf`, [`streaming_chunk_size`] bytes) is
/// allocated **once** by the caller and reused for every chunk via
/// `JByteArray::set_region` + `OutputStream.write(byte[], int, int)`
/// — the previous implementation allocated a fresh exact-size Java
/// array per chunk (`byte_array_from_slice`).  Axum body frames are
/// unbounded in size, so frames larger than the buffer are written
/// in buffer-sized segments.
///
/// NOTE: when request pull and response push run concurrently
/// (bidirectional streaming), each side MUST own a **separate**
/// buffer — they execute on different threads.
pub fn make_push_closure(
    jvm: jni::JavaVM,
    stream: Global<JObject<'static>>,
    buf: Global<jni::objects::JByteArray<'static>>,
) -> impl FnMut(&[u8]) + Send + 'static {
    let chunk_size = streaming_chunk_size();
    // Latches once the Java OutputStream errors (e.g. the client
    // disconnected mid-download): subsequent frames become a cheap
    // no-op instead of repeatedly crossing JNI to write into a broken
    // sink and clearing the resulting exception every time.
    let mut failed = false;
    move |chunk: &[u8]| {
        if failed {
            return;
        }
        // Daemon-attach this thread once, cached in TLS, instead of
        // attach+detach per frame; the helper wraps the body in a fresh
        // local-reference frame.
        let outcome = with_cached_daemon_env(&jvm, |env| -> jni::errors::Result<()> {
            let arr: &jni::objects::JByteArray<'_> = buf.as_ref();
            for seg in chunk.chunks(chunk_size) {
                // SAFETY: `u8` and `i8` (JNI's `jbyte`) have
                // identical size/alignment; this views the
                // segment as the signed slice `set_region`
                // expects.  `seg.len() <= chunk_size` (max
                // 8 MiB) so it always fits both the buffer
                // and `i32`.
                let seg_i8 =
                    unsafe { std::slice::from_raw_parts(seg.as_ptr().cast::<i8>(), seg.len()) };
                arr.set_region(env, 0, seg_i8)?;
                let len = i32::try_from(seg.len())
                    .expect("segment length bounded by streaming_chunk_size");
                call_output_stream_write(env, &stream, &buf, len)?;
                // Any IOException thrown by write() is left
                // pending on the env; clear it so subsequent
                // chunks on the same thread aren't poisoned.
                if env.exception_check() {
                    env.exception_clear();
                }
            }
            Ok(())
        });
        if outcome.is_err() {
            failed = true;
        }
    }
}

pub fn call_header_consumer(
    env: &mut jni::Env<'_>,
    consumer: &Global<JObject<'static>>,
    header_bytes: &[u8],
) -> jni::errors::Result<()> {
    env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
        let arr = env.byte_array_from_slice(header_bytes)?;
        let arr_obj: JObject = arr.into();
        call_consumer_accept(env, consumer, &arr_obj)?;
        if env.exception_check() {
            env.exception_clear();
        }
        Ok(())
    })
}

/// Call `CompletableFuture.complete(byte[])` and clear any pending
/// JNI exception so the worker thread is left clean for subsequent
/// dispatches.
pub fn complete_future(
    env: &mut jni::Env<'_>,
    future: &Global<JObject<'static>>,
    bytes: &[u8],
) -> jni::errors::Result<()> {
    let arr = env.byte_array_from_slice(bytes)?;
    let arr_obj: JObject = arr.into();
    call_future_complete(env, future, &arr_obj)?;
    // Always clear any leftover exception (e.g. if Java's
    // complete() threw via a buggy whenComplete handler): we MUST
    // NOT leave the attached thread in a faulted state because
    // subsequent JNI calls will misbehave silently.
    if env.exception_check() {
        env.exception_clear();
    }
    Ok(())
}
