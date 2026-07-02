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

use std::ops::ControlFlow;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jni::ids::JMethodID;
use jni::objects::{JClass, JObject};
use jni::refs::Global;
use jni::signature::{MethodSignature, Primitive, ReturnType};
use jni::strings::JNIStr;
use jni::sys::{jint, jvalue};
use jni::{JValue, JValueOwned, jni_sig, jni_str};

use crate::daemon_env::with_cached_daemon_env_no_frame;
use crate::jni_impl::{clear_pending_exception, streaming_chunk_size};

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

/// Process-global cache of the four `java.*` callback method IDs.
///
/// **Single-JVM-per-process invariant (deliberate).** The cached `JMethodID`s
/// and their pinning `Global<JClass>` are JVM-local, and this `OnceLock` is
/// keyed only by the process — NOT by `JavaVM`. This is sound because:
///
/// * HotSpot supports exactly one JVM per OS process — `JNI_CreateJavaVM`
///   fails on a second call — so a second `JavaVM` whose IDs could differ
///   cannot exist alongside the cached one.
/// * Every cached class (`InputStream`, `OutputStream`, `Consumer`,
///   `CompletableFuture`) is a bootstrap `java.*` class that never unloads,
///   so the cached IDs stay valid for the process lifetime.
/// * [`crate::daemon_env`] separately stores and compares the raw `JavaVM`
///   pointer on every cached-env reuse, so a thread attached to a *different*
///   VM cannot even obtain a live `Env` to reach this cache.
///
/// A per-call `JavaVM` check is intentionally NOT added: it would require a
/// `GetJavaVM` JNI call on every streaming chunk — the exact per-chunk JNI
/// cost this cache exists to eliminate — to guard against a multi-JVM
/// configuration the platform already forbids. Trading hot-path throughput
/// for that guard would be a net regression.
enum MethodCacheState {
    Ready(MethodCache),
    Failed,
}

static METHOD_CACHE: OnceLock<MethodCacheState> = OnceLock::new();

const ZERO_READ_YIELD_THRESHOLD: u32 = 16;
const ZERO_READ_BACKOFF_STEP: Duration = Duration::from_micros(50);
const ZERO_READ_BACKOFF_CAP: Duration = Duration::from_millis(1);

fn zero_read_backoff(consecutive_empty_reads: u32) -> Option<Duration> {
    let over_threshold = consecutive_empty_reads.checked_sub(ZERO_READ_YIELD_THRESHOLD)?;
    // `over_threshold + 1` would panic in debug (or wrap in release) at
    // `consecutive_empty_reads == u32::MAX` — unreachable in practice (the
    // backoff caps at 1 ms, so ~4 G empty reads × ≥ 50 µs ≈ 60 hours of
    // pure backoff per stream), but aligned here with the panic-free
    // discipline the rest of this FFI-adjacent file documents
    // explicitly (cf. `make_push_closure`'s `i32::try_from(...).unwrap_or(chunk_size_i32)`
    // and `make_pull_closure`'s `usize::try_from` fallback).
    let step_count = over_threshold.saturating_add(1);
    Some((ZERO_READ_BACKOFF_STEP * step_count).min(ZERO_READ_BACKOFF_CAP))
}

fn method_cache(env: &mut jni::Env<'_>) -> Option<&'static MethodCache> {
    if let Some(state) = METHOD_CACHE.get() {
        return match state {
            MethodCacheState::Ready(cache) => Some(cache),
            MethodCacheState::Failed => None,
        };
    }

    let Ok(cache) = MethodCache::resolve(env) else {
        // Cache init is best-effort.  If class lookup, method lookup,
        // or global-ref promotion fails, clear only that init-time
        // exception and run the exact old string-based call path below.
        clear_pending_exception(env);
        let _ = METHOD_CACHE.set(MethodCacheState::Failed);
        return None;
    };

    let _ = METHOD_CACHE.set(MethodCacheState::Ready(cache));
    match METHOD_CACHE.get() {
        Some(MethodCacheState::Ready(cache)) => Some(cache),
        Some(MethodCacheState::Failed) | None => None,
    }
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

/// Check-and-clear variant of [`clear_pending_exception`]: returns
/// whether an exception was pending (and cleared).
///
/// Every cached-`JMethodID` call site above (`call_input_stream_read`,
/// `call_output_stream_write`, `call_consumer_accept`) uses
/// `call_method_unchecked` on the fast path, which returns `Ok` while
/// leaving a thrown Java exception PENDING on the thread instead of
/// surfacing it as `Err` (only the checked fallback surfaces it).  The
/// three streaming closures below (`make_pull_closure`,
/// `make_push_closure`, `call_header_consumer`) have to convert that
/// pending exception into an abort with per-caller-specific return
/// values (`RequestChunk::Error` for pull; `JavaException` `Err` for
/// push / header-consumer), so they share the check-clear preamble
/// while keeping their return type distinct.  Centralising the
/// check-clear here means a future policy change (e.g. logging the
/// exception before clearing, or `exception_describe()` in debug
/// builds) needs to touch ONE site instead of drifting across three.
/// `#[inline]` folds it back into each caller so codegen matches the
/// previous inline expression.
#[inline]
fn take_pending_exception(env: &mut jni::Env<'_>) -> bool {
    if env.exception_check() {
        env.exception_clear();
        true
    } else {
        false
    }
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
    stream: Arc<Global<JObject<'static>>>,
    buf: Arc<Global<jni::objects::JByteArray<'static>>>,
) -> impl FnMut() -> vespera_inprocess::RequestChunk + Send + 'static {
    use vespera_inprocess::RequestChunk;
    let chunk_size = streaming_chunk_size();
    let mut consecutive_empty_reads = 0_u32;
    move || -> RequestChunk {
        // Daemon-attach this (Tokio `spawn_blocking`) thread once,
        // cached in TLS, instead of attach+detach per chunk.  No local
        // frame: the body below creates no JNI local refs (cached
        // unchecked `read` call + raw `get_region` into a Rust Vec), so
        // the per-chunk frame would be pure overhead.
        let result: jni::errors::Result<RequestChunk> =
            with_cached_daemon_env_no_frame(&jvm, |env| {
                let n = call_input_stream_read(env, &stream, &buf)?;
                // The cached fast path calls `read()` via `call_method_unchecked`,
                // which does NOT surface a thrown exception as `Err` — it returns a
                // garbage `n` with the exception left pending. A thrown `read()`
                // must ABORT the request body so a truncated upload is rejected,
                // and must never be misread as EOF (`n < 0`) or a chunk. (The
                // checked fallback in `call_input_stream_read` already aborts via
                // `?`; acting on the pending exception here gives the unchecked
                // path identical semantics instead of interpreting the garbage `n`.)
                if take_pending_exception(env) {
                    return Ok(RequestChunk::Error);
                }
                // InputStream.read(byte[]) contract (mirrored in the
                // VesperaBridge javadoc): -1 = EOF, 0 = empty read that
                // MUST be retried.  The inprocess producer skips empty
                // chunks and keeps pulling, so report `0` as an empty
                // chunk rather than end-of-stream.
                if n < 0 {
                    consecutive_empty_reads = 0;
                    return Ok(RequestChunk::End);
                }
                if n == 0 {
                    consecutive_empty_reads = consecutive_empty_reads.saturating_add(1);
                    if let Some(delay) = zero_read_backoff(consecutive_empty_reads) {
                        std::thread::sleep(delay);
                    }
                    return Ok(RequestChunk::Data(Vec::new()));
                }
                consecutive_empty_reads = 0;
                // `n > 0` here (the `< 0` and `== 0` cases returned above), so a
                // positive `jint` always fits `usize`.  Avoid a panic site on
                // this FFI hot path: an impossible conversion failure aborts the
                // request body (`RequestChunk::Error`) instead of unwinding
                // across the JNI boundary.
                let Ok(n) = usize::try_from(n) else {
                    return Ok(RequestChunk::Error);
                };
                // `InputStream.read(byte[])` MUST return at most the buffer
                // length; a larger value is a contract violation (a buggy or
                // hostile stream).  Treat it as stream corruption and ABORT
                // the request body instead of silently clamping it to a
                // "valid" read — clamping would feed a truncated / mis-sized
                // chunk downstream and accept a corrupted upload as complete.
                if n > chunk_size {
                    return Ok(RequestChunk::Error);
                }
                // Copy the n bytes just read into the Java buffer straight into
                // uninitialised capacity — no zero-fill to immediately overwrite.
                let arr: &jni::objects::JByteArray<'_> = buf.as_ref().as_ref();
                let data = crate::jni_buf::read_byte_array_region(env, arr, n)?;
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
    buf: Arc<Global<jni::objects::JByteArray<'static>>>,
) -> impl FnMut(&[u8]) -> ControlFlow<()> + Send + 'static {
    let chunk_size = streaming_chunk_size();
    // `chunk_size` is config-clamped to <= 8 MiB (see config::MAX_STREAMING_CHUNK_BYTES),
    // so every segment length (<= chunk_size) fits an `i32`.  Precompute the
    // saturating bound once so the per-segment length conversion below needs no
    // panic site; the `unwrap_or` fallback is the buffer size (never exceeds it),
    // so it stays write-safe even if the clamp invariant were ever broken.
    let chunk_size_i32 = i32::try_from(chunk_size).unwrap_or(i32::MAX);
    // Latches once the Java OutputStream errors (e.g. the client
    // disconnected mid-download): subsequent frames become a cheap
    // no-op instead of repeatedly crossing JNI to write into a broken
    // sink and clearing the resulting exception every time.
    let mut failed = false;
    move |chunk: &[u8]| {
        if failed {
            return ControlFlow::Break(());
        }
        // Daemon-attach this thread once, cached in TLS, instead of
        // attach+detach per frame.  No local frame: the body below
        // creates no JNI local refs (cached unchecked `write` call +
        // `set_region`), so the per-chunk frame would be pure overhead.
        let outcome = with_cached_daemon_env_no_frame(&jvm, |env| -> jni::errors::Result<()> {
            let arr: &jni::objects::JByteArray<'_> = buf.as_ref().as_ref();
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
                // seg.len() <= chunk_size <= 8 MiB always fits i32; the
                // `unwrap_or(chunk_size_i32)` fallback is unreachable but keeps
                // this FFI hot path panic-free (and write-safe: the fallback is
                // the buffer length) if the clamp invariant ever changes.
                let len = i32::try_from(seg.len()).unwrap_or(chunk_size_i32);
                call_output_stream_write(env, &stream, &buf, len)?;
                // The cached fast path calls `write()` via `call_method_unchecked`,
                // which leaves a thrown `write()` (e.g. the client disconnected
                // mid-download) PENDING instead of surfacing it as `Err`. Clear it
                // AND propagate so the `failed` latch engages and we STOP writing
                // the remaining segments/frames into a broken sink — instead of
                // clearing it and futilely streaming the rest of the body to a
                // dead stream. (The checked fallback already latches via `?`.)
                if take_pending_exception(env) {
                    return Err(jni::errors::Error::JavaException);
                }
            }
            Ok(())
        });
        if outcome.is_err() {
            failed = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
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
        let result = call_consumer_accept(env, consumer, &arr_obj);
        // `call_consumer_accept`'s cached `call_method_unchecked` fast path
        // returns `Ok` with a thrown `Consumer.accept` left PENDING (only the
        // checked fallback surfaces it as `Err`). A throwing header consumer
        // is a FAILURE and MUST be reported as `Err`, exactly like the cached
        // `read`/`write` paths convert their pending exception to an
        // abort/`Err`. Otherwise the caller's `.is_ok()` records
        // `header_sent = true` for a header the Java side never accepted, and
        // the body keeps streaming over a failed header instead of aborting.
        // Scrub on BOTH paths so the thread is left clean, then fail if a
        // throw was detected.
        if take_pending_exception(env) {
            return Err(jni::errors::Error::JavaException);
        }
        result?;
        Ok(())
    })
}

/// Run the cold-path scrub → `byte_array_from_slice` → `invoke` → scrub
/// recipe shared by [`call_header_consumer_local`] and [`complete_future_local`].
///
/// The five-step contract used by the cold setup-failure / fallback paths is:
/// 1. Scrub any pending exception from the failed JNI call that routed
///    us here — `byte_array_from_slice` must NOT be invoked with an
///    exception in flight.
/// 2. Allocate the Java `byte[]` for `bytes`.  If allocation ITSELF
///    leaves a pending exception (e.g. OOM), scrub it before surfacing
///    the error so it does not leak into the caller's next JNI call.
/// 3. Invoke `invoke(env, &arr_obj)` to deliver the array to its
///    Java-side consumer (`Consumer.accept` / `CompletableFuture.complete`
///    today; the FnOnce makes this open to future cold-path consumers
///    without re-duplicating the scrub framing).
/// 4. Scrub on BOTH success and failure so a throwing consumer cannot
///    poison the thread's next JNI call.
/// 5. Propagate `invoke`'s result verbatim.
///
/// Keeping the scrub contract in one place is the whole point — both
/// existing call sites previously inlined the recipe with comments
/// re-explaining the invariant per copy, exactly the shape that drifts
/// when someone updates only one site.
fn cold_path_byte_array_call<F>(
    env: &mut jni::Env<'_>,
    bytes: &[u8],
    invoke: F,
) -> jni::errors::Result<()>
where
    F: FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<()>,
{
    clear_pending_exception(env);
    let arr = match env.byte_array_from_slice(bytes) {
        Ok(arr) => arr,
        Err(e) => {
            clear_pending_exception(env);
            return Err(e);
        }
    };
    let arr_obj: JObject = arr.into();
    let result = invoke(env, &arr_obj);
    clear_pending_exception(env);
    result
}

/// Fire `Consumer.accept(byte[])` through a **local** consumer reference,
/// for the cold setup-failure / fallback paths of the streaming-with-header
/// dispatchers that run on the JNI entry thread (where the original
/// `header_consumer` local ref is still valid).
///
/// Uses the checked `call_method` (no cached `JMethodID`) — these paths are
/// rare (oversized / failed ingress read, or a global-ref / VM-promotion /
/// buffer-checkout failure during setup). Crucially it does NOT promote the
/// consumer to a `Global` first, so it still delivers the mandatory single
/// header callback even when the very allocation that would promote it is what
/// failed — upholding the "header consumer invoked exactly once on every code
/// path" contract so the Java caller never hangs.
pub fn call_header_consumer_local(
    env: &mut jni::Env<'_>,
    consumer: &JObject<'_>,
    header_bytes: &[u8],
) -> jni::errors::Result<()> {
    cold_path_byte_array_call(env, header_bytes, |env, arr_obj| {
        env.call_method(
            consumer,
            jni_str!("accept"),
            jni_sig!("(Ljava/lang/Object;)V"),
            &[JValue::Object(arr_obj)],
        )
        .map(|_| ())
    })
}

/// Complete a `CompletableFuture` via a **local** reference, for the
/// cold error / fallback paths of `dispatchAsync` that run on the JNI
/// entry thread (where the original `future` local ref is still valid).
///
/// Uses the checked `call_method` — these paths are rare (oversized
/// request, JNI conversion failure, VM-promotion / scheduling failure),
/// so they do not need the cached-`JMethodID` fast path that
/// [`complete_future`] uses for the per-dispatch hot completion on the
/// worker thread.  This lets `dispatchAsync` hold a **single** `Global`
/// ref (for the spawned task) instead of a second one kept solely for
/// these on-thread completions.
///
/// On a failed completion the `CompletableFuture` is left uncompleted by
/// THIS helper; the caller treats the `Err` as a failed best-effort
/// completion rather than hanging on a poisoned thread.  The original
/// exception is intentionally discarded — we are converting a JNI failure
/// into a best-effort `500` completion, not surfacing the inner throw.
pub fn complete_future_local(
    env: &mut jni::Env<'_>,
    future: &JObject<'_>,
    bytes: &[u8],
) -> jni::errors::Result<()> {
    cold_path_byte_array_call(env, bytes, |env, arr_obj| {
        env.call_method(
            future,
            jni_str!("complete"),
            jni_sig!("(Ljava/lang/Object;)Z"),
            &[JValue::Object(arr_obj)],
        )
        .map(|_| ())
    })
}

/// Best-effort `InputStream.close()` — invoked after a bidirectional
/// dispatch finishes to unblock a request producer parked in a blocking
/// `read`, so the dispatch cannot hang on a stuck upload.  Any pending
/// exception (e.g. an `IOException` from closing an already-broken
/// stream) is cleared so the thread is left clean.
pub fn close_input_stream(
    env: &mut jni::Env<'_>,
    stream: &Global<JObject<'static>>,
) -> jni::errors::Result<()> {
    let result = env.call_method(stream, jni_str!("close"), jni_sig!("()V"), &[]);
    // Scrub a pending exception (e.g. an `IOException` from closing an
    // already-broken stream) on BOTH success and failure — capturing the
    // result and clearing BEFORE `?` so a throwing `close()` still leaves the
    // thread clean, matching `complete_future{,_local}`'s self-contained
    // contract (the prior `?`-before-clear returned early on a throw).
    clear_pending_exception(env);
    result?;
    Ok(())
}

/// Call `CompletableFuture.complete(byte[])` and clear any pending
/// JNI exception so the worker thread is left clean for subsequent
/// dispatches.
pub fn complete_future(
    env: &mut jni::Env<'_>,
    future: &Global<JObject<'static>>,
    bytes: &[u8],
) -> jni::errors::Result<()> {
    // Capture the result instead of `?`-propagating so the exception clear
    // below runs on EVERY path. The prior early `?` on byte_array_from_slice
    // / complete() returned before the clear, leaking a pending exception
    // onto the (pooled, daemon-attached) worker thread for the next dispatch
    // — contradicting this function's own "left clean" contract.
    let result = match env.byte_array_from_slice(bytes) {
        Ok(arr) => {
            let arr_obj: JObject = arr.into();
            call_future_complete(env, future, &arr_obj)
        }
        Err(e) => Err(e),
    };
    // Always clear any leftover exception (e.g. if Java's complete() threw
    // via a buggy whenComplete handler): we MUST NOT leave the attached
    // thread in a faulted state because subsequent JNI calls will misbehave
    // silently.
    clear_pending_exception(env);
    result
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::zero_read_backoff;

    #[test]
    fn zero_read_backoff_starts_after_repeated_empty_reads() {
        // Given: a JNI InputStream that repeatedly reports empty reads.
        // When: the count reaches the JNI-side threshold.
        // Then: the pull closure stays on the fast path below the threshold,
        // then sleeps with a tiny capped backoff instead of busy-yielding forever.
        assert_eq!(zero_read_backoff(15), None);
        assert_eq!(zero_read_backoff(16), Some(Duration::from_micros(50)));
        assert_eq!(zero_read_backoff(35), Some(Duration::from_millis(1)));
    }
}
