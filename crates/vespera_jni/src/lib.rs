//! JNI bridge for vespera.
//!
//! # Quick start
//!
//! ```ignore
//! vespera::jni_app!(create_app);
//! ```
//!
//! The [`jni_app!`] macro generates `JNI_OnLoad` which registers your
//! router factory via [`vespera_inprocess::register_app`].  The JNI
//! dispatch symbol is exported by this crate, matching the fixed Java
//! class `com.devfive.vespera.bridge.VesperaBridge`.

#![allow(unsafe_code)]
#![cfg(not(tarpaulin_include))]

pub use jni;
pub use vespera_inprocess;

/// Generate the `JNI_OnLoad` export that registers a single (default)
/// app.  Backward-compatible sugar for the single-app case; new code
/// targeting multiple apps should use [`jni_apps!`] directly.
///
/// ```ignore
/// vespera::jni_app!(create_app);
/// ```
///
/// Expands to `register_app(factory)` inside the generated
/// `JNI_OnLoad`.  The resulting router is reachable from Java
/// without an `X-Vespera-App` header (or with the header set to
/// `"_default"`).
#[macro_export]
macro_rules! jni_app {
    ($factory:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            $crate::vespera_inprocess::register_app($factory);
            $crate::jni::sys::JNI_VERSION_1_8
        }
    };
}

/// Generate the `JNI_OnLoad` export that registers **multiple named
/// apps** in a single declaration.  This is the primary multi-app
/// entry point — exactly one `JNI_OnLoad` per cdylib is generated,
/// regardless of how many apps you register.
///
/// ```ignore
/// vespera::jni_apps! {
///     "admin"  => admin_app,
///     "public" => public_app,
/// }
/// ```
///
/// Each `name` must be a string literal matching the validation rules
/// in [`register_app_named`] (non-empty, ≤ 64 bytes, alphanumeric +
/// `_` `-`).  Each `factory` is an expression evaluating to a
/// `Fn() -> Router + Send + Sync + 'static` (typically a `pub fn`
/// path).
///
/// From the Java side, the request's `X-Vespera-App` header
/// (configurable) selects which app receives the dispatch.  Requests
/// without the header are routed to the default app (registered via
/// [`jni_app!`] or `register_app`); requests naming an unregistered
/// app receive a 404 wire response.
///
/// # Composition
///
/// JNI requires exactly one `JNI_OnLoad` per cdylib.  Use `jni_apps!`
/// (or `jni_app!`) **once** in the cdylib's root module; assemble
/// factories from submodules into that single invocation.  Using
/// `jni_app!` and `jni_apps!` together — or `jni_apps!` more than
/// once — will produce a duplicate-symbol link error.
///
/// [`register_app_named`]: vespera_inprocess::register_app_named
#[macro_export]
macro_rules! jni_apps {
    ( $( $name:literal => $factory:expr ),+ $(,)? ) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            $(
                $crate::vespera_inprocess::register_app_named($name, $factory);
            )+
            $crate::jni::sys::JNI_VERSION_1_8
        }
    };
}

// Everything below requires a JVM — excluded from coverage.
#[cfg(not(tarpaulin_include))]
mod jni_impl {
    use std::sync::LazyLock;

    use jni::EnvUnowned;
    use jni::errors::ThrowRuntimeExAndDefault;
    use jni::objects::{Global, JByteArray, JByteBuffer, JClass, JObject, JValue};
    use jni::sys::{jbyteArray, jint};
    use jni::{jni_sig, jni_str};

    /// Multi-threaded Tokio runtime shared across all JNI calls.
    pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create Tokio runtime")
    });

    /// Per-chunk buffer size for streaming dispatches.
    ///
    /// Resolved once per process by
    /// [`vespera_inprocess::streaming_chunk_bytes`] (default 64 KiB;
    /// override via the `VESPERA_STREAMING_CHUNK_BYTES` env var or the
    /// `configureStreaming0` JNI setter called from
    /// `VesperaBridge.init()`).  Large enough to amortise JNI call
    /// overhead, small enough to keep memory bounded for multi-GB
    /// streams.  Subsequent calls are a single atomic load.
    fn streaming_chunk_size() -> usize {
        vespera_inprocess::streaming_chunk_bytes()
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.configureStreaming0(int, int) -> void`
    ///
    /// Seeds the process-wide streaming configuration **before the
    /// first dispatch**.  Values `<= 0` leave the corresponding
    /// setting untouched (env var / default applies).  Calls after
    /// the configuration is fixed (first dispatch already ran, or a
    /// previous call set it) are silently ignored — the JNI side has
    /// no use for the failure signal beyond logging, which Java owns.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_configureStreaming0<
        'local,
    >(
        _unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        chunk_bytes: jint,
        channel_capacity: jint,
    ) {
        if let Ok(bytes) = usize::try_from(chunk_bytes)
            && bytes > 0
        {
            let _ = vespera_inprocess::set_streaming_chunk_bytes(bytes);
        }
        if let Ok(slots) = usize::try_from(channel_capacity)
            && slots > 0
        {
            let _ = vespera_inprocess::set_streaming_channel_capacity(slots);
        }
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchBytes(byte[]) -> byte[]`
    ///
    /// **Synchronous** binary wire-format JNI entry point.  Blocks the
    /// calling thread until the Rust dispatch completes.  Wraps the
    /// entire pipeline in `catch_unwind` so a panic anywhere produces
    /// a valid wire-format `500` response with a plain-text body —
    /// JVM never sees an unwinding stack across the FFI boundary.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchBytes<'local>(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        request_bytes: JByteArray<'local>,
    ) -> jbyteArray {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let Ok(input) = env.convert_byte_array(&request_bytes) else {
                    let err = vespera_inprocess::error_wire(
                        400,
                        "invalid input byte array (JNI conversion failed)",
                    );
                    return Ok(env.byte_array_from_slice(&err)?.into());
                };

                let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    vespera_inprocess::dispatch_from_bytes(input, &RUNTIME)
                }))
                .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

                Ok(env.byte_array_from_slice(&response)?.into())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }

    /// Sentinel for [`Java_..._dispatchDirect`]: the response (or its
    /// required size) cannot be represented in the `jint` return value
    /// (> `i32::MAX` bytes).
    ///
    /// `jint::MIN` is the only value the `-(required_size)` protocol can
    /// never produce: `required_size <= i32::MAX`, so the most negative
    /// legitimate return is `-(i32::MAX) == jint::MIN + 1`.
    const DIRECT_UNREPRESENTABLE: jint = jint::MIN;

    // Compile-time proof that the sentinel cannot collide with any
    // legitimate `-(required_size)` value.
    const _: () = assert!(DIRECT_UNREPRESENTABLE < -i32::MAX);

    /// Copy `response` into the caller's direct out buffer.
    ///
    /// Returns:
    /// * `>= 0` — bytes written (`response` fit entirely)
    /// * `< 0`  — `-(required_size)`: nothing written, caller must retry
    ///   with a buffer of at least `required_size` bytes
    /// * [`DIRECT_UNREPRESENTABLE`] — response exceeds `i32::MAX` bytes
    ///   and cannot be expressed in the return-code protocol
    ///
    /// # Safety contract (upheld by the caller)
    ///
    /// `out_addr` must point to a writable region of at least `out_cap`
    /// bytes that stays valid for the duration of this call (a JNI
    /// direct buffer pinned by the live `JByteBuffer` local ref).
    fn write_response_to_out(out_addr: *mut u8, out_cap: usize, response: &[u8]) -> jint {
        if response.len() <= out_cap {
            // SAFETY: `response.len() <= out_cap` and the caller
            // guarantees `out_addr..out_addr+out_cap` is writable.
            // Source and destination cannot overlap: `response` is a
            // Rust-owned Vec, the destination is a Java direct buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(response.as_ptr(), out_addr, response.len());
            }
            // Java buffer capacities are jint-bounded, so len <= cap
            // always fits i32.
            jint::try_from(response.len()).unwrap_or(DIRECT_UNREPRESENTABLE)
        } else {
            jint::try_from(response.len()).map_or(DIRECT_UNREPRESENTABLE, |required| -required)
        }
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchDirect0(ByteBuffer, int, ByteBuffer) -> int`
    /// (private native; the public Java wrapper `dispatchDirect` validates
    /// buffer directness before crossing JNI)
    ///
    /// **Direct-buffer** synchronous dispatch — the zero-JNI-region-copy
    /// sibling of [`Java_...dispatchBytes`].
    ///
    /// Contract (mirrored in the Java wrapper's javadoc):
    /// * `in_buf` / `out_buf` MUST be **direct** `ByteBuffer`s.  The
    ///   Java wrapper enforces this before crossing JNI; non-direct
    ///   buffers reaching this symbol produce a thrown
    ///   `RuntimeException` (the jni crate surfaces a null direct
    ///   address as `Err`).
    /// * The wire request is read from `in_buf[0..in_len]` — explicit
    ///   `in_len`, **never** the buffer's position/limit (eliminates
    ///   the classic "forgot to flip()" corruption).
    /// * Return `>= 0`: a complete wire response was written to
    ///   `out_buf[0..n]`.
    /// * Return `< 0`: `-(required_size)` — the response did not fit.
    ///   `out_buf` contents are **undefined** (a prefix may have been
    ///   written).  `required_size` is exact, but retrying re-runs the
    ///   dispatch, so the Java side only auto-retries idempotent
    ///   methods.
    /// * `Integer.MIN_VALUE`: response size exceeds `i32::MAX`.
    ///
    /// Compared with `dispatchBytes`, this path removes BOTH JNI
    /// region copies (Java `byte[]` ↔ Rust), the per-call Java heap
    /// array allocations, AND — via
    /// [`vespera_inprocess::dispatch_into_async`] — the intermediate
    /// response `Vec`: on the success path the wire header and each
    /// body frame are written straight into `out_buf`.  One plain
    /// native memcpy remains on the request side (axum's `Body`
    /// requires `'static` ownership), plus the per-frame copies of the
    /// response body.  `422` responses are materialised internally to
    /// preserve `validation_errors` hoisting.
    ///
    /// # Safety invariants (comment-locked)
    ///
    /// 1. `in_buf` / `out_buf` stay rooted as live local refs for the
    ///    whole call — HotSpot neither moves nor frees the backing
    ///    memory of a direct buffer while its object is reachable.
    /// 2. The raw addresses derived from them are used **only within
    ///    this function body** — never captured by closures, spawned
    ///    tasks, or returned structs.
    /// 3. The input slice is copied into a Rust-owned `Vec` *before*
    ///    dispatch, so nothing borrowed from the buffer outlives the
    ///    read.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchDirect0<'local>(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        in_buf: JByteBuffer<'local>,
        in_len: jint,
        out_buf: JByteBuffer<'local>,
    ) -> jint {
        unowned_env
            .with_env(|env| -> jni::errors::Result<jint> {
                // Err here (null address ⇒ heap buffer, or JVM trouble)
                // is thrown as RuntimeException via the resolve below —
                // defense in depth behind the Java-side isDirect() check.
                let in_addr = env.get_direct_buffer_address(&in_buf)?;
                let in_cap = env.get_direct_buffer_capacity(&in_buf)?;
                let out_addr = env.get_direct_buffer_address(&out_buf)?;
                let out_cap = env.get_direct_buffer_capacity(&out_buf)?;

                // Validate in_len against the buffer's real capacity —
                // all failures still produce a valid wire response in
                // `out_buf`, per the dispatch* family contract.
                let input = match usize::try_from(in_len) {
                    Ok(len) if len <= in_cap => {
                        // SAFETY: invariants 1–3 above; `len <= in_cap`
                        // bounds the read inside the direct buffer.
                        unsafe { std::slice::from_raw_parts(in_addr, len) }.to_vec()
                    }
                    _ => {
                        let err = vespera_inprocess::error_wire(
                            400,
                            "invalid in_len (negative or exceeds buffer capacity)",
                        );
                        return Ok(write_response_to_out(out_addr, out_cap, &err));
                    }
                };

                let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // SAFETY: invariants 1–2 above — `out_addr` points
                    // to `out_cap` writable bytes of a direct buffer
                    // pinned by the live `out_buf` local ref; the Java
                    // caller is blocked for the whole call, so the
                    // region is exclusively ours; the slice never
                    // escapes this closure.
                    let out = unsafe { std::slice::from_raw_parts_mut(out_addr, out_cap) };
                    RUNTIME.block_on(vespera_inprocess::dispatch_into_async(input, out))
                }));

                let code = match dispatched {
                    Ok(vespera_inprocess::DirectWriteResult::Complete(n)) => {
                        // n <= out_cap, and Java buffer capacities are
                        // jint-bounded, so this always fits i32.
                        jint::try_from(n).unwrap_or(DIRECT_UNREPRESENTABLE)
                    }
                    Ok(vespera_inprocess::DirectWriteResult::Overflow(required)) => {
                        jint::try_from(required).map_or(DIRECT_UNREPRESENTABLE, |r| -r)
                    }
                    Err(_) => {
                        let err = vespera_inprocess::error_wire(500, "panic in Rust engine");
                        write_response_to_out(out_addr, out_cap, &err)
                    }
                };
                Ok(code)
            })
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchAsync(CompletableFuture<byte[]>, byte[]) -> void`
    ///
    /// **Asynchronous** binary wire-format JNI entry point.  Returns
    /// immediately after spawning the dispatch on the shared Tokio
    /// runtime.  Completes the supplied `CompletableFuture<byte[]>`
    /// from a runtime worker thread once the response is ready.
    ///
    /// Contract (always-complete):
    /// - **success** → `future.complete(responseBytes)`
    /// - **JNI conversion failure** → `future.complete(error_wire(400, ...))`
    /// - **Rust panic / handler crash** → `future.complete(error_wire(500, "panic in Rust engine"))`
    ///   The future is always completed with a valid wire response —
    ///   it is never left dangling, even on internal errors.
    ///
    /// Cancellation: Java's `future.cancel(true)` does NOT abort the
    /// in-flight Rust task in this iteration (defer to follow-up).
    /// Java callers may still observe cancellation via `future.isCancelled()`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchAsync<'local>(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        future_obj: JObject<'local>,
        request_bytes: JByteArray<'local>,
    ) {
        // Best-effort: any error inside with_env aborts the dispatch
        // (future will dangle on the Java side — only happens if we
        // can't even promote the future to a GlobalRef, which would
        // mean the JVM is already in trouble).
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            // 1. Promote CompletableFuture to Global so it survives
            //    across the tokio task boundary.
            let future_global: Global<JObject<'static>> = env.new_global_ref(&future_obj)?;

            // 2. Try to convert the input byte array.  On failure,
            //    complete the future synchronously with the error wire
            //    and return early — no async work needed.
            let Ok(input) = env.convert_byte_array(&request_bytes) else {
                let err = vespera_inprocess::error_wire(
                    400,
                    "invalid input byte array (JNI conversion failed)",
                );
                let _ = complete_future(env, &future_global, &err);
                return Ok(());
            };

            // 3. Snapshot the JavaVM (Send + Sync) so we can re-attach
            //    the tokio worker thread once the dispatch completes.
            let jvm = env.get_java_vm()?;

            // 4. Fire-and-forget on the runtime.  An inner tokio::spawn
            //    converts any panic in dispatch_from_bytes_async into
            //    a JoinError, guaranteeing always-complete semantics.
            RUNTIME.spawn(async move {
                let response = tokio::spawn(vespera_inprocess::dispatch_from_bytes_async(input))
                    .await
                    .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

                // Re-attach to JVM on this worker thread; subsequent
                // dispatches on the same thread will hit the TLS fast
                // path (cheap).
                let _ = jvm.attach_current_thread(|env| -> jni::errors::Result<()> {
                    complete_future(env, &future_global, &response)
                });
            });

            Ok(())
        });
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchStreaming(byte[], OutputStream) -> byte[]`
    ///
    /// **Streaming** JNI entry point.  Drives the dispatch
    /// synchronously like [`Java_...dispatchBytes`], but emits the
    /// response body chunk-by-chunk by calling `outputStream.write(byte[])`
    /// for each chunk axum produces — no full-body materialisation on
    /// either the Rust or JVM side.
    ///
    /// Returns the wire-format **header only** (`[u32 BE header_len |
    /// header JSON]`) — the body is delivered through the
    /// `OutputStream` argument while the dispatch is in flight.
    /// Callers (e.g. Spring `StreamingResponseBody`) read the header
    /// first to commit the HTTP status + response headers, then
    /// continue serving the streamed body bytes.
    ///
    /// Failure modes mirror [`Java_...dispatchBytes`]: malformed wire,
    /// version mismatch, no app registered, or Rust panic produce a
    /// regular `error_wire(...)` response (header + small body) and
    /// the `OutputStream` is **not** written to.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreaming<
        'local,
    >(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        request_bytes: JByteArray<'local>,
        output_stream: JObject<'local>,
    ) -> jbyteArray {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let Ok(input) = env.convert_byte_array(&request_bytes) else {
                    let err = vespera_inprocess::error_wire(
                        400,
                        "invalid input byte array (JNI conversion failed)",
                    );
                    return Ok(env.byte_array_from_slice(&err)?.into());
                };

                // Promote the OutputStream to Global so we can call
                // .write() from a different attached thread inside
                // the streaming callback.
                let stream_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
                let jvm = env.get_java_vm()?;

                // One reusable Java chunk buffer for the whole stream.
                let push_buf_local = env.new_byte_array(streaming_chunk_size())?;
                let push_buf: Global<jni::objects::JByteArray<'static>> =
                    env.new_global_ref(&push_buf_local)?;

                let header_bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    RUNTIME.block_on(vespera_inprocess::dispatch_streaming_async(
                        input,
                        make_push_closure(jvm, stream_global, push_buf),
                    ))
                }))
                .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

                Ok(env.byte_array_from_slice(&header_bytes)?.into())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchFullStreaming(byte[], InputStream, OutputStream) -> byte[]`
    ///
    /// **Bidirectional streaming** JNI entry point.  Reads the request
    /// body chunk-by-chunk from `inputStream.read(byte[])` and emits
    /// response body chunks via `outputStream.write(byte[])` — neither
    /// side ever materialises the full body in memory, so 1 GiB
    /// uploads with 1 GiB downloads run in O(chunk_size) RAM.
    ///
    /// Returns the wire-format **header only** (`[u32 BE header_len |
    /// header JSON]`); the response body was delivered through
    /// `outputStream`.
    ///
    /// Wire envelope contract:
    /// - `headerBytes` is a wire-format request **without a body**
    ///   (just the 4-byte length prefix + JSON header).  Send the
    ///   request body via `inputStream`, not embedded in this buffer.
    /// - `inputStream.read(byte[])` semantics: returns `-1` on EOF,
    ///   `0` for an empty read (will be retried), or `>0` for the
    ///   number of bytes read into the supplied buffer.
    ///
    /// Failure modes mirror [`Java_...dispatchStreaming`]: malformed
    /// wire / unknown version / no app / Rust panic produce a normal
    /// `error_wire(...)` response in the returned bytes and neither
    /// stream is touched.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchFullStreaming<
        'local,
    >(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        header_bytes: JByteArray<'local>,
        input_stream: JObject<'local>,
        output_stream: JObject<'local>,
    ) -> jbyteArray {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let Ok(header_input) = env.convert_byte_array(&header_bytes) else {
                    let err = vespera_inprocess::error_wire(
                        400,
                        "invalid header byte array (JNI conversion failed)",
                    );
                    return Ok(env.byte_array_from_slice(&err)?.into());
                };

                let input_global: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
                let output_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
                let jvm = env.get_java_vm()?;

                // One reusable Java chunk buffer PER SIDE — pull and
                // push run concurrently on different threads, so each
                // direction owns its own global-ref'd buffer.
                let pull_buf_local = env.new_byte_array(streaming_chunk_size())?;
                let pull_buf: Global<jni::objects::JByteArray<'static>> =
                    env.new_global_ref(&pull_buf_local)?;
                let push_buf_local = env.new_byte_array(streaming_chunk_size())?;
                let push_buf: Global<jni::objects::JByteArray<'static>> =
                    env.new_global_ref(&push_buf_local)?;

                // Closures capture clones of the JavaVM and Globals;
                // both types are Send+Sync.
                let pull_jvm = jvm.clone();
                let pull_global = input_global;
                let push_jvm = jvm;
                let push_global = output_global;

                let header_response =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        RUNTIME.block_on(vespera_inprocess::dispatch_bidirectional_streaming(
                            header_input,
                            // Pull request body chunks from Java InputStream.
                            // Runs on a tokio blocking thread (spawn_blocking
                            // inside dispatch_bidirectional_streaming).
                            make_pull_closure(pull_jvm, pull_global, pull_buf),
                            // Push response body chunks to Java OutputStream.
                            // Runs on the tokio worker driving the dispatch.
                            make_push_closure(push_jvm, push_global, push_buf),
                        ))
                    }))
                    .unwrap_or_else(|_| vespera_inprocess::error_wire(500, "panic in Rust engine"));

                Ok(env.byte_array_from_slice(&header_response)?.into())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchStreamingWithHeader(byte[], Consumer<byte[]>, OutputStream) -> void`
    ///
    /// Same as [`Java_...dispatchStreaming`] but emits the wire-format
    /// response header via `headerConsumer.accept(byte[])` **before**
    /// the first body byte reaches `outputStream`.  This lets
    /// Spring-style `HttpServletResponse` controllers commit status
    /// and headers while the response is still uncommitted.
    ///
    /// `headerConsumer` is invoked exactly once on every code path
    /// (success or error); the bytes are a normal wire-format header
    /// (length-prefixed JSON).  On error `outputStream` is not
    /// touched.
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
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            let Ok(input) = env.convert_byte_array(&request_bytes) else {
                let err = vespera_inprocess::error_wire(
                    400,
                    "invalid input byte array (JNI conversion failed)",
                );
                let _ = call_header_consumer(env, &env.new_global_ref(&header_consumer)?, &err);
                return Ok(());
            };

            let header_global: Global<JObject<'static>> = env.new_global_ref(&header_consumer)?;
            let stream_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
            let jvm = env.get_java_vm()?;

            // One reusable Java chunk buffer for the whole stream.
            let push_buf_local = env.new_byte_array(streaming_chunk_size())?;
            let push_buf: Global<jni::objects::JByteArray<'static>> =
                env.new_global_ref(&push_buf_local)?;

            // Panic safety: catch_unwind absorbs Rust panics so the
            // JVM never sees an unwinding stack across the FFI
            // boundary.  If the panic happens AFTER the header
            // callback fires (the common case — most panics are in
            // axum handlers), Spring's response is already partially
            // committed; we have no way to recover that.  If the
            // panic happens BEFORE the header callback fires (very
            // rare — e.g. wire parse), the Java side will see a
            // dangling controller; document that follow-up callers
            // should set a timeout.
            let _panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let header_for_cb = header_global;
                let jvm_for_cb = jvm.clone();
                let push = make_push_closure(jvm, stream_global, push_buf);
                RUNTIME.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
                    input,
                    |header_bytes: &[u8]| {
                        let _ = jvm_for_cb.attach_current_thread(
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                call_header_consumer(env, &header_for_cb, header_bytes)
                            },
                        );
                    },
                    push,
                ));
            }));

            Ok(())
        });
    }

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatchFullStreamingWithHeader(byte[], Consumer<byte[]>, InputStream, OutputStream) -> void`
    ///
    /// Bidirectional streaming with the same header-callback contract
    /// as [`Java_...dispatchStreamingWithHeader`].  Request body
    /// pulled from `inputStream`, response header emitted via
    /// `headerConsumer.accept(byte[])` once axum produces status +
    /// headers, then response body chunks streamed to `outputStream`.
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
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            let Ok(header_input) = env.convert_byte_array(&header_bytes_in) else {
                let err = vespera_inprocess::error_wire(
                    400,
                    "invalid header byte array (JNI conversion failed)",
                );
                let _ = call_header_consumer(env, &env.new_global_ref(&header_consumer)?, &err);
                return Ok(());
            };

            let header_global: Global<JObject<'static>> = env.new_global_ref(&header_consumer)?;
            let input_global: Global<JObject<'static>> = env.new_global_ref(&input_stream)?;
            let output_global: Global<JObject<'static>> = env.new_global_ref(&output_stream)?;
            let jvm = env.get_java_vm()?;

            // One reusable Java chunk buffer PER SIDE — pull and push
            // run concurrently on different threads.
            let pull_buf_local = env.new_byte_array(streaming_chunk_size())?;
            let pull_buf: Global<jni::objects::JByteArray<'static>> =
                env.new_global_ref(&pull_buf_local)?;
            let push_buf_local = env.new_byte_array(streaming_chunk_size())?;
            let push_buf: Global<jni::objects::JByteArray<'static>> =
                env.new_global_ref(&push_buf_local)?;

            let pull_jvm = jvm.clone();
            let pull_global = input_global;
            let push_jvm = jvm.clone();
            let push_global = output_global;
            let header_jvm = jvm;
            let header_for_cb = header_global;

            // See dispatchStreamingWithHeader: panic absorbed silently,
            // recovery semantics depend on which side of the header
            // callback the panic landed.
            let _panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RUNTIME.block_on(
                    vespera_inprocess::dispatch_bidirectional_streaming_with_header(
                        header_input,
                        make_pull_closure(pull_jvm, pull_global, pull_buf),
                        make_push_closure(push_jvm, push_global, push_buf),
                        |header_bytes: &[u8]| {
                            let _ = header_jvm.attach_current_thread(
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    call_header_consumer(env, &header_for_cb, header_bytes)
                                },
                            );
                        },
                    ),
                );
            }));

            Ok(())
        });
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
    fn make_pull_closure(
        jvm: jni::JavaVM,
        stream: Global<JObject<'static>>,
        buf: Global<jni::objects::JByteArray<'static>>,
    ) -> impl FnMut() -> Option<Vec<u8>> + Send + 'static {
        // Resolved once at closure-build time — zero per-chunk cost.
        // Identical to the buffer's allocation size by OnceLock
        // construction (the config is process-fixed after first read).
        let chunk_size = streaming_chunk_size();
        move || -> Option<Vec<u8>> {
            let result: jni::errors::Result<Option<Vec<u8>>> = jvm.attach_current_thread(|env| {
                env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                    let n = env
                        .call_method(
                            &stream,
                            jni_str!("read"),
                            jni_sig!("([B)I"),
                            &[JValue::Object(buf.as_ref())],
                        )?
                        .i()?;
                    if env.exception_check() {
                        env.exception_clear();
                    }
                    // InputStream.read(byte[]) contract (mirrored in the
                    // VesperaBridge javadoc): -1 = EOF, 0 = empty read that
                    // MUST be retried.  The inprocess producer skips empty
                    // chunks and keeps pulling, so report `0` as an empty
                    // chunk rather than end-of-stream.
                    if n < 0 {
                        return Ok(None);
                    }
                    if n == 0 {
                        return Ok(Some(Vec::new()));
                    }
                    let n = usize::try_from(n).unwrap_or(0).min(chunk_size);
                    let mut data = vec![0u8; n];
                    // SAFETY: `u8` and `i8` (JNI's `jbyte`) have
                    // identical size/alignment; this views the
                    // freshly allocated buffer as the signed slice
                    // `get_byte_array_region` expects.
                    let data_i8 = unsafe {
                        std::slice::from_raw_parts_mut(data.as_mut_ptr().cast::<i8>(), n)
                    };
                    let arr: &jni::objects::JByteArray<'_> = buf.as_ref();
                    arr.get_region(env, 0, data_i8)?;
                    Ok(Some(data))
                })
            });
            result.ok().flatten()
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
    fn make_push_closure(
        jvm: jni::JavaVM,
        stream: Global<JObject<'static>>,
        buf: Global<jni::objects::JByteArray<'static>>,
    ) -> impl FnMut(&[u8]) + Send + 'static {
        // Resolved once at closure-build time — zero per-chunk cost.
        let chunk_size = streaming_chunk_size();
        move |chunk: &[u8]| {
            let _ =
                jvm.attach_current_thread(|env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                    env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                        let arr: &jni::objects::JByteArray<'_> = buf.as_ref();
                        for seg in chunk.chunks(chunk_size) {
                            // SAFETY: `u8` and `i8` (JNI's `jbyte`) have
                            // identical size/alignment; this views the
                            // segment as the signed slice `set_region`
                            // expects.  `seg.len() <= chunk_size` (max
                            // 8 MiB) so it always fits both the buffer
                            // and `i32`.
                            let seg_i8 = unsafe {
                                std::slice::from_raw_parts(seg.as_ptr().cast::<i8>(), seg.len())
                            };
                            arr.set_region(env, 0, seg_i8)?;
                            let len = i32::try_from(seg.len())
                                .expect("segment length bounded by streaming_chunk_size");
                            env.call_method(
                                &stream,
                                jni_str!("write"),
                                jni_sig!("([BII)V"),
                                &[
                                    JValue::Object(buf.as_ref()),
                                    JValue::Int(0),
                                    JValue::Int(len),
                                ],
                            )?;
                            // Any IOException thrown by write() is left
                            // pending on the env; clear it so subsequent
                            // chunks on the same thread aren't poisoned.
                            if env.exception_check() {
                                env.exception_clear();
                            }
                        }
                        Ok(())
                    })
                });
        }
    }

    fn call_header_consumer(
        env: &mut jni::Env<'_>,
        consumer: &Global<JObject<'static>>,
        header_bytes: &[u8],
    ) -> jni::errors::Result<()> {
        env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
            let arr = env.byte_array_from_slice(header_bytes)?;
            let arr_obj: JObject = arr.into();
            env.call_method(
                consumer,
                jni_str!("accept"),
                jni_sig!("(Ljava/lang/Object;)V"),
                &[JValue::Object(&arr_obj)],
            )?;
            if env.exception_check() {
                env.exception_clear();
            }
            Ok(())
        })
    }

    /// Call `CompletableFuture.complete(byte[])` and clear any pending
    /// JNI exception so the worker thread is left clean for subsequent
    /// dispatches.
    fn complete_future(
        env: &mut jni::Env<'_>,
        future: &Global<JObject<'static>>,
        bytes: &[u8],
    ) -> jni::errors::Result<()> {
        let arr = env.byte_array_from_slice(bytes)?;
        let arr_obj: JObject = arr.into();
        env.call_method(
            future,
            jni_str!("complete"),
            jni_sig!("(Ljava/lang/Object;)Z"),
            &[JValue::Object(&arr_obj)],
        )?;
        // Always clear any leftover exception (e.g. if Java's
        // complete() threw via a buggy whenComplete handler): we MUST
        // NOT leave the attached thread in a faulted state because
        // subsequent JNI calls will misbehave silently.
        if env.exception_check() {
            env.exception_clear();
        }
        Ok(())
    }

    #[cfg(test)]
    mod direct_tests {
        use super::write_response_to_out;

        #[test]
        fn response_fits_returns_len_and_writes_bytes() {
            let mut out = vec![0u8; 16];
            let response = b"hello wire";
            let n = write_response_to_out(out.as_mut_ptr(), out.len(), response);
            assert_eq!(n, 10);
            assert_eq!(&out[..10], response);
        }

        #[test]
        fn exact_fit_boundary() {
            let mut out = vec![0u8; 4];
            let n = write_response_to_out(out.as_mut_ptr(), out.len(), b"abcd");
            assert_eq!(n, 4);
            assert_eq!(&out[..], b"abcd");
        }

        #[test]
        fn overflow_returns_negative_required_size_and_writes_nothing() {
            let mut out = vec![0xAAu8; 4];
            let n = write_response_to_out(out.as_mut_ptr(), out.len(), b"too large");
            assert_eq!(n, -9);
            assert_eq!(
                &out[..],
                &[0xAA; 4],
                "overflow must not touch the out buffer"
            );
        }

        #[test]
        fn zero_capacity_overflow() {
            let mut out: Vec<u8> = Vec::new();
            let n = write_response_to_out(out.as_mut_ptr(), 0, b"x");
            assert_eq!(n, -1);
        }
    }
}
