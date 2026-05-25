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
    use jni::objects::{Global, JByteArray, JClass, JObject, JValue};
    use jni::sys::jbyteArray;
    use jni::{jni_sig, jni_str};

    /// Multi-threaded Tokio runtime shared across all JNI calls.
    pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create Tokio runtime")
    });

    /// Per-chunk buffer size for streaming dispatches (16 KiB — large
    /// enough to amortise JNI call overhead, small enough to keep
    /// memory bounded for multi-GB streams).
    const STREAMING_CHUNK_SIZE: usize = 16 * 1024;

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
                    .unwrap_or_else(|_| {
                        vespera_inprocess::error_wire(500, "panic in Rust engine")
                    });

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
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreaming<'local>(
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

                let header_bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    RUNTIME.block_on(vespera_inprocess::dispatch_streaming_async(
                        input,
                        |chunk: &[u8]| {
                            // Per-chunk: attach (cheap on subsequent
                            // calls — TLS fast path) + push a local
                            // frame to keep the local-ref table bounded
                            // even for streams with thousands of chunks.
                            let _ = jvm.attach_current_thread(
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    env.with_local_frame::<_, _, jni::errors::Error>(
                                        8,
                                        |env| {
                                            let arr = env.byte_array_from_slice(chunk)?;
                                            let arr_obj: JObject = arr.into();
                                            env.call_method(
                                                &stream_global,
                                                jni_str!("write"),
                                                jni_sig!("([B)V"),
                                                &[JValue::Object(&arr_obj)],
                                            )?;
                                            // Any IOException thrown by write() is left
                                            // pending on the env; clear it so subsequent
                                            // chunks on the same thread aren't poisoned.
                                            if env.exception_check() {
                                                env.exception_clear();
                                            }
                                            Ok(())
                                        },
                                    )
                                },
                            );
                        },
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
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchFullStreaming<'local>(
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

                // Closures capture clones of the JavaVM and Globals;
                // both types are Send+Sync.
                let pull_jvm = jvm.clone();
                let pull_global = input_global;
                let push_jvm = jvm;
                let push_global = output_global;

                let header_response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    RUNTIME.block_on(vespera_inprocess::dispatch_bidirectional_streaming(
                        header_input,
                        // Pull request body chunks from Java InputStream.
                        // Runs on a tokio blocking thread (spawn_blocking
                        // inside dispatch_bidirectional_streaming).
                        move || -> Option<Vec<u8>> {
                            let result: jni::errors::Result<Option<Vec<u8>>> = pull_jvm
                                .attach_current_thread(|env| {
                                    env.with_local_frame::<_, _, jni::errors::Error>(
                                        8,
                                        |env| {
                                            let arr = env.new_byte_array(STREAMING_CHUNK_SIZE)?;
                                            let n = env
                                                .call_method(
                                                    &pull_global,
                                                    jni_str!("read"),
                                                    jni_sig!("([B)I"),
                                                    &[JValue::Object(arr.as_ref())],
                                                )?
                                                .i()?;
                                            if env.exception_check() {
                                                env.exception_clear();
                                            }
                                            if n <= 0 {
                                                return Ok(None);
                                            }
                                            let mut data = env.convert_byte_array(&arr)?;
                                            data.truncate(usize::try_from(n).unwrap_or(0));
                                            Ok(Some(data))
                                        },
                                    )
                                });
                            result.ok().flatten()
                        },
                        // Push response body chunks to Java OutputStream.
                        // Runs on the tokio worker driving the dispatch.
                        |chunk: &[u8]| {
                            let _ = push_jvm.attach_current_thread(
                                |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                    env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                                        let arr = env.byte_array_from_slice(chunk)?;
                                        let arr_obj: JObject = arr.into();
                                        env.call_method(
                                            &push_global,
                                            jni_str!("write"),
                                            jni_sig!("([B)V"),
                                            &[JValue::Object(&arr_obj)],
                                        )?;
                                        if env.exception_check() {
                                            env.exception_clear();
                                        }
                                        Ok(())
                                    })
                                },
                            );
                        },
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
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchStreamingWithHeader<'local>(
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
                let stream_for_cb = stream_global;
                let jvm_for_cb = jvm;
                RUNTIME.block_on(vespera_inprocess::dispatch_streaming_with_header_async(
                    input,
                    |header_bytes: &[u8]| {
                        let _ = jvm_for_cb.attach_current_thread(
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                call_header_consumer(env, &header_for_cb, header_bytes)
                            },
                        );
                    },
                    |chunk: &[u8]| {
                        let _ = jvm_for_cb.attach_current_thread(
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                                    write_chunk_to_stream(env, &stream_for_cb, chunk)
                                })
                            },
                        );
                    },
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
                RUNTIME.block_on(vespera_inprocess::dispatch_bidirectional_streaming_with_header(
                    header_input,
                    move || -> Option<Vec<u8>> {
                        let result: jni::errors::Result<Option<Vec<u8>>> = pull_jvm
                            .attach_current_thread(|env| {
                                env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                                    let arr = env.new_byte_array(STREAMING_CHUNK_SIZE)?;
                                    let n = env
                                        .call_method(
                                            &pull_global,
                                            jni_str!("read"),
                                            jni_sig!("([B)I"),
                                            &[JValue::Object(arr.as_ref())],
                                        )?
                                        .i()?;
                                    if env.exception_check() {
                                        env.exception_clear();
                                    }
                                    if n <= 0 {
                                        return Ok(None);
                                    }
                                    let mut data = env.convert_byte_array(&arr)?;
                                    data.truncate(usize::try_from(n).unwrap_or(0));
                                    Ok(Some(data))
                                })
                            });
                        result.ok().flatten()
                    },
                    |chunk: &[u8]| {
                        let _ = push_jvm.attach_current_thread(
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                env.with_local_frame::<_, _, jni::errors::Error>(8, |env| {
                                    write_chunk_to_stream(env, &push_global, chunk)
                                })
                            },
                        );
                    },
                    |header_bytes: &[u8]| {
                        let _ = header_jvm.attach_current_thread(
                            |env: &mut jni::Env<'_>| -> jni::errors::Result<()> {
                                call_header_consumer(env, &header_for_cb, header_bytes)
                            },
                        );
                    },
                ));
            }));

            Ok(())
        });
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

    fn write_chunk_to_stream(
        env: &mut jni::Env<'_>,
        stream: &Global<JObject<'static>>,
        chunk: &[u8],
    ) -> jni::errors::Result<()> {
        let arr = env.byte_array_from_slice(chunk)?;
        let arr_obj: JObject = arr.into();
        env.call_method(
            stream,
            jni_str!("write"),
            jni_sig!("([B)V"),
            &[JValue::Object(&arr_obj)],
        )?;
        if env.exception_check() {
            env.exception_clear();
        }
        Ok(())
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
}
