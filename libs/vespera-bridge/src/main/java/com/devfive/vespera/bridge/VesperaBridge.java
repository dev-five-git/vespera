package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.function.Consumer;

/**
 * JNI bridge to any Rust cdylib built with vespera's JNI feature.
 *
 * <p>This class owns only the pieces that must stay bound to the
 * {@code com.devfive.vespera.bridge.VesperaBridge} symbol name — the
 * {@code native} methods (whose JNI symbols are
 * {@code Java_com_devfive_vespera_bridge_VesperaBridge_*}), native-library
 * loading, and the public dispatch API.  The pure-Java helpers live in
 * sibling classes: wire request encoding / response decoding in
 * {@link VesperaWireCodec}, and the per-thread direct-buffer pool in
 * {@link VesperaDirectBufferPool}.  The public methods here delegate to
 * them, so callers see an unchanged surface.
 *
 * <p><strong>Wire format</strong> — both request and response use the
 * same layout:
 * <pre>
 *   bytes 0..4    : u32 BE = header_json byte length N
 *   bytes 4..4+N  : UTF-8 JSON header
 *                     (request)  { "v":1, "method", "path", "query"?,
 *                                  "headers"?, "app"?, "remote_addr"?, "scheme"? }
 *                     (response) { "v":1, "status", "headers", "metadata" }
 *   bytes 4+N..   : raw body bytes (no encoding applied)
 * </pre>
 *
 * <p><strong>Usage</strong>: {@code VesperaBridge.init("rust_jni_demo");} — the
 * proxy controller ({@link VesperaProxyController}) is autoconfigured by Spring Boot.
 */
public class VesperaBridge {

    @FunctionalInterface
    public interface HeaderSink {
        void put(String lowerName, String value);
    }

    /**
     * Streams request headers into the encoder; the optional defaults add the
     * {@code remote_addr} / {@code scheme} / trusted {@code principal} (never from client headers) wire fields ({@code null} = omitted).
     */
    @FunctionalInterface
    public interface HeaderSource {
        void writeTo(HeaderSink sink);

        /** Client socket address as {@code ip:port} ({@code [ip]:port} for IPv6), or {@code null}. */
        default String remoteAddr() {
            return null;
        }

        /** {@code http} / {@code https}, or {@code null}. */
        default String scheme() {
            return null;
        }

        default VesperaPrincipal principal() {
            return null;
        }
    }

    private static volatile boolean loaded = false;
    private static volatile boolean shutDown = false;
    /** Name passed to the first successful {@link #init(String)} — used to
     *  reject a later re-init with a <em>different</em> library name. */
    private static String loadedLibraryName;

    @FunctionalInterface
    interface NativeLibraryLoader {
        void load(String libraryName);
    }

    /**
     * Decoded wire-format response.
     *
     * <p>The {@code body} component is a zero-copy, read-only
     * {@link ByteBuffer} view over the original wire response array.
     * Its position is {@code 0} and its limit is the body length.  The
     * view does not expose {@link ByteBuffer#array()} access, so callers
     * that genuinely need an owned {@code byte[]} should use
     * {@link #bodyBytes()}, which materialises a copy on demand.
     *
     * @param status            HTTP status code from the upstream router
     * @param headers           response headers; each value is either a
     *                          {@link String} (single-valued) or a
     *                          {@link List List&lt;String&gt;}
     *                          (multi-valued, e.g. {@code set-cookie})
     * @param metadata          vespera metadata (e.g. {@code version})
     * @param body              read-only raw response body view
     * @param validationErrors  Vespera-validation failures hoisted from
     *                          a {@code 422} JSON body so callers can
     *                          read them without a second JSON parse.
     *                          {@code null} when the response is not a
     *                          422 or doesn't carry the {@code
     *                          validation_errors} wire header field.
     *                          Each entry typically has {@code path},
     *                          {@code code}, and {@code message} keys.
     */
    public record DecodedResponse(
            int status,
            Map<String, Object> headers,
            Map<String, String> metadata,
            ByteBuffer body,
            List<Map<String, Object>> validationErrors) {

        public DecodedResponse {
            Objects.requireNonNull(body, "body");
            if (!body.isReadOnly() || body.position() != 0) {
                body = body.slice().asReadOnlyBuffer();
            }
        }

        /**
         * Return a fresh read-only duplicate of the response body view.
         * The returned buffer is positioned at {@code 0} with
         * {@code limit()} equal to the body length.
         */
        @Override
        public ByteBuffer body() {
            return body.asReadOnlyBuffer();
        }

        /**
         * Materialise the response body as an owned byte array.
         *
         * <p>This method copies the bytes from the zero-copy body view;
         * use it at API boundaries that require {@code byte[]}.
         */
        public byte[] bodyBytes() {
            ByteBuffer view = body.asReadOnlyBuffer();
            byte[] bytes = new byte[view.remaining()];
            view.get(bytes);
            return bytes;
        }
    }

    /**
     * Initialize the Rust engine.  Tries bundled (JAR-embedded) first,
     * falls back to {@code java.library.path}.
     *
     * <p>Engine configuration ({@code vespera.streaming.*},
     * {@code vespera.runtime.*}, {@code vespera.limits.*},
     * {@code vespera.dispatch.timeoutMillis}, {@code vespera.async.maxInFlight}
     * system properties, the matching {@code VESPERA_*} environment variables,
     * or the {@code configure*} methods) is seeded <strong>before the first
     * dispatch</strong> and fixed for the process lifetime afterwards; see the
     * README "Streaming tuning" / limits tables.  A value the Rust side could
     * not apply (already fixed) is logged as a warning.
     *
     * <p>The native library must come from the same {@code major.minor}
     * release as this JAR: a mismatch fails here with an
     * {@link IllegalStateException} unless
     * {@code -Dvespera.native.versionCheck=warn} (log only) or {@code off}.
     *
     * @param libraryName Cargo crate name (e.g. {@code "rust_jni_demo"})
     */
    public static synchronized void init(String libraryName) {
        Objects.requireNonNull(libraryName, "libraryName");
        if (loaded) {
            // Re-init with the SAME library is a no-op (friendly for test
            // harness resets / repeated Spring context starts). A DIFFERENT
            // name is a bug — a JVM process loads exactly one vespera cdylib
            // for its lifetime — so surface it instead of silently keeping
            // the first library and dispatching to the wrong Rust app.
            if (!loadedLibraryName.equals(libraryName)) {
                throw new IllegalStateException(
                        "VesperaBridge is already initialised with native library '"
                        + loadedLibraryName + "' and cannot be re-initialised with a "
                        + "different library '" + libraryName + "'.");
            }
            return;
        }
        loadNativeLibrary(libraryName, VesperaNativeLoader::loadBundled, System::loadLibrary);
        // Bridge JAR <-> native library release check (VesperaVersions); a
        // major.minor mismatch throws before the library is marked loaded.
        VesperaVersions.verify(VesperaBridge::nativeVersion0);
        // Mark the native library as loaded once System.load succeeded and the
        // versions match: the post-load configuration hooks below may still
        // throw, but a later init() must not load the same cdylib again.
        loaded = true;
        loadedLibraryName = libraryName;
        // Apply pending config (set via configure* before init); pending
        // values beat system properties (Rust-side setter > env > default).
        VesperaNativeConfig.applyAfterLoad();
    }

    static void loadNativeLibrary(
            String libraryName,
            NativeLibraryLoader bundledLoader,
            NativeLibraryLoader systemLoader) {
        try {
            try {
                bundledLoader.load(libraryName);
            } catch (VesperaNativeLoader.BundledNativeAbsent absent) {
                systemLoader.load(libraryName);
            }
        } catch (UnsatisfiedLinkError failure) {
            throw VesperaNativeLoader.withDeploymentGuidance(failure, libraryName);
        }
    }

    /** Whether {@link #init(String)} has loaded the native library in this class loader. */
    public static boolean isInitialized() {
        return loaded;
    }

    /** Whether {@link #shutdown(Duration)} stopped the native runtime (dispatches answer 503). */
    public static boolean isShutdown() {
        return shutDown;
    }

    /**
     * Gracefully shut the Rust runtime down, waiting up to {@code timeout} for
     * in-flight tasks. <strong>Irreversible for the JVM</strong> (a native
     * library cannot be reloaded in-process): later dispatches answer
     * {@code 503}. Returns {@code false} when not initialised, already shut
     * down, or the native library predates the hook.
     */
    public static synchronized boolean shutdown(Duration timeout) {
        Objects.requireNonNull(timeout, "timeout");
        if (!loaded) {
            return false;
        }
        long millis = timeout.isNegative() ? 0 : timeout.toMillis();
        try {
            boolean stopped = shutdown0(millis);
            shutDown |= stopped;
            return stopped;
        } catch (UnsatisfiedLinkError olderNativeLibrary) {
            return false;
        }
    }

    static native boolean shutdown0(long timeoutMillis);

    /** UTF-8 {@code CARGO_PKG_VERSION} of the native library (see {@link VesperaVersions}). */
    static native byte[] nativeVersion0();

    /**
     * Configure streaming tuning parameters for the Rust-side dispatch
     * engine.  <strong>Call before {@link #init(String)}</strong> for
     * guaranteed precedence (values are stored pending and applied right
     * after the native library loads, before any dispatch); calling after
     * init applies immediately.
     *
     * <p>Precedence (first hit wins, then process-fixed): this method &gt;
     * system properties ({@code vespera.streaming.chunkBytes} /
     * {@code vespera.streaming.channelCapacity}) &gt; environment variables
     * ({@code VESPERA_STREAMING_CHUNK_BYTES} /
     * {@code VESPERA_STREAMING_CHANNEL_CAPACITY}) &gt; defaults
     * (256 KiB chunk, 16 channel slots).
     *
     * @param chunkBytes per-chunk buffer size for streaming dispatches
     * @param channelCapacity bound of the bidirectional request-body
     *                        channel in slots
     * <p>Returns {@code void} for binary compatibility with 0.5.x callers; a
     * value the Rust side had already fixed is logged as a warning.
     *
     * @throws IllegalArgumentException if {@code chunkBytes} is outside
     *         [4096, 8388608] (4 KiB – 8 MiB) or {@code channelCapacity}
     *         is outside [1, 1024]
     */
    public static synchronized void configureStreaming(int chunkBytes, int channelCapacity) {
        if (chunkBytes < 4096 || chunkBytes > 8388608) {
            throw new IllegalArgumentException(
                    "chunkBytes " + chunkBytes
                            + " out of range [4096, 8388608] (4 KiB – 8 MiB)");
        }
        if (channelCapacity < 1 || channelCapacity > 1024) {
            throw new IllegalArgumentException(
                    "channelCapacity " + channelCapacity + " out of range [1, 1024]");
        }
        VesperaNativeConfig.setStreaming(chunkBytes, channelCapacity, loaded);
    }

    /**
     * Configure the shared Tokio runtime (same pending-config pattern and
     * precedence as {@link #configureStreaming}).  Must take effect before the
     * runtime starts (first dispatch); values {@code <= 0} leave a setting
     * untouched.
     *
     * @param workerThreads      worker threads (clamped 1–1024; default:
     *                           logical CPUs)
     * @param maxBlockingThreads blocking-pool cap (clamped 1–8192; default 512)
     * @return {@code false} when a value could not be applied (logged)
     */
    public static synchronized boolean configureRuntime(int workerThreads, int maxBlockingThreads) {
        boolean workers = VesperaNativeConfig.RUNTIME_WORKER_THREADS.set(workerThreads, loaded);
        boolean blocking =
                VesperaNativeConfig.RUNTIME_MAX_BLOCKING_THREADS.set(maxBlockingThreads, loaded);
        return workers && blocking;
    }

    /**
     * Configure the Rust-side size limits (pending-config pattern, see
     * {@link #configureStreaming}).  {@code 0} = unlimited; a negative value
     * leaves that limit untouched.
     *
     * @param maxRequestBytes          buffered request cap (default 64 MiB;
     *                                 over → {@code 413})
     * @param maxStreamingRequestBytes bidirectional-streaming request-body
     *                                 cap (default unlimited; over → {@code 413})
     * @param maxBufferedResponseBytes buffered response cap (default 256 MiB;
     *                                 over → {@code 500})
     * @return {@code false} when a value could not be applied (logged)
     */
    public static synchronized boolean configureLimits(
            long maxRequestBytes, long maxStreamingRequestBytes, long maxBufferedResponseBytes) {
        boolean request = VesperaNativeConfig.MAX_REQUEST_BYTES.set(maxRequestBytes, loaded);
        boolean streaming = VesperaNativeConfig.MAX_STREAMING_REQUEST_BYTES
                .set(maxStreamingRequestBytes, loaded);
        boolean response = VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES
                .set(maxBufferedResponseBytes, loaded);
        return request && streaming && response;
    }

    /**
     * Configure the per-dispatch deadline in milliseconds ({@code 0} =
     * disabled, the default; negative = untouched).  A buffered dispatch
     * (SYNC / ASYNC / DIRECT) is bounded as a whole and answers {@code 504}
     * on expiry; a streaming dispatch is bounded only until its response
     * header (time-to-first-header, {@code 504} when it expires) — once the
     * header is committed the body streams without a total deadline.
     *
     * @return {@code false} when the value could not be applied (logged)
     */
    public static synchronized boolean configureDispatchTimeout(long timeoutMillis) {
        return VesperaNativeConfig.DISPATCH_TIMEOUT_MILLIS.set(timeoutMillis, loaded);
    }

    /**
     * Configure the {@link #dispatchAsync} in-flight cap ({@code 0} =
     * unlimited, default 4096; negative = untouched).  Calls beyond the cap
     * complete immediately with a retryable {@code 503} wire response.
     *
     * @return {@code false} when the value could not be applied (logged)
     */
    public static synchronized boolean configureAsyncMaxInFlight(int maxInFlight) {
        return VesperaNativeConfig.ASYNC_MAX_IN_FLIGHT.set(maxInFlight, loaded);
    }

    /**
     * Clear all vespera-bridge buffers retained by the <em>current</em> Java
     * thread. This is for servlet-container shutdown/redeploy hooks that want
     * to release ThreadLocal-held app-class objects and direct buffers from
     * container worker threads. Normal request handling should not call it;
     * per-request clearing would defeat the hot-path pools.
     */
    public static void clearCurrentThreadBuffers() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        VesperaWireCodec.clearCurrentThreadBuffers();
        WireHeaderReader.clearCurrentThreadBuffers();
    }

    // Optional configuration hooks (see VesperaNativeConfig).  Each returns
    // whether the Rust side applied the value; a native library that predates
    // a hook throws UnsatisfiedLinkError, which VesperaNativeConfig tolerates.
    // Values meaning "untouched": <= 0 for the streaming / runtime hooks,
    // negative for the limit / timeout / async hooks (where 0 = unlimited).
    static native boolean configureStreaming0(int chunkBytes, int channelCapacity);

    static native boolean configureRuntime0(int workerThreads);

    static native boolean configureMaxBlockingThreads0(int maxBlockingThreads);

    static native boolean configureMaxRequestBytes0(long maxBytes);

    static native boolean configureMaxStreamingRequestBytes0(long maxBytes);

    static native boolean configureMaxBufferedResponseBytes0(long maxBytes);

    static native boolean configureDispatchTimeout0(long timeoutMillis);

    static native boolean configureAsyncMaxInFlight0(int maxInFlight);

    /**
     * Dispatch a wire-format HTTP-like request through the Rust axum
     * router (<strong>synchronous</strong> — blocks the calling
     * thread).  See {@link VesperaBridge class-level docs} for the
     * wire layout.
     *
     * @param wireRequest length-prefixed binary wire request
     * @return length-prefixed binary wire response
     */
    public static native byte[] dispatchBytes(byte[] wireRequest);

    /**
     * Asynchronous variant of {@link #dispatchBytes(byte[])}.  Returns
     * immediately after spawning the dispatch on Rust's Tokio runtime;
     * the supplied {@link CompletableFuture} is completed with the
     * wire-format response bytes from a runtime worker thread.
     *
     * <p>Contract (always-complete): the future is always completed
     * with a valid wire response.  Panics in the Rust handler are
     * converted to a `500` wire response; JNI conversion failures to
     * a `400` wire response.  The future is never left dangling.
     *
     * <p>Cancellation propagates to Rust: when {@code future} completes
     * exceptionally ({@code cancel(..)}, {@code orTimeout(..)}, an external
     * {@code completeExceptionally(..)}) the in-flight Rust handler is
     * dropped at its next {@code .await} and its
     * {@code vespera.async.maxInFlight} slot is released at once.
     *
     * <p><strong>Threading contract (IMPORTANT):</strong> the future is
     * completed on a Rust Tokio <em>runtime worker thread</em>, so any
     * <em>non-async</em> continuation ({@code thenApply}, {@code thenAccept},
     * {@code whenComplete}, &hellip;) runs <strong>inline on that worker</strong>.
     * Therefore:
     * <ul>
     *   <li>attach heavy or blocking continuations with the {@code *Async}
     *       variants ({@code thenApplyAsync}, {@code whenCompleteAsync}, &hellip;)
     *       on your own {@link java.util.concurrent.Executor}; and</li>
     *   <li>never call a blocking vespera dispatch ({@link #dispatchBytes(byte[])}
     *       / {@link #dispatchDirect(java.nio.ByteBuffer, int, java.nio.ByteBuffer)})
     *       from an inline continuation &mdash; that nests a blocking call inside
     *       the runtime worker and degrades to a {@code 500} wire response.</li>
     * </ul>
     * Completing the future off the worker (a {@code spawn_blocking} hand-off)
     * was measured at ~16&times; the per-dispatch cost, so the worker-thread
     * completion is kept and this contract is documented instead &mdash; the same
     * approach Netty and async HTTP clients take. The autoconfigured Spring proxy
     * never selects this async path (it uses DIRECT / SYNC / streaming), so this
     * applies only to callers composing {@link CompletableFuture}s directly.
     *
     * @param future        the future to complete with the wire response
     * @param wireRequest   length-prefixed binary wire request
     */
    public static void dispatchAsync(CompletableFuture<byte[]> future, byte[] wireRequest) {
        AsyncDispatch.dispatch(future, wireRequest);
    }

    /**
     * Convenience wrapper around {@link #dispatchAsync} that allocates
     * the {@link CompletableFuture} and returns it.
     *
     * @param wireRequest length-prefixed binary wire request
     * @return future that resolves to the wire-format response bytes
     */
    public static CompletableFuture<byte[]> dispatch(byte[] wireRequest) {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        dispatchAsync(future, wireRequest);
        return future;
    }

    /**
     * <strong>Streaming</strong> binary wire-format JNI dispatch.  The
     * dispatch runs synchronously on the calling thread (like
     * {@link #dispatchBytes(byte[])}) but emits the response body
     * <strong>chunk-by-chunk</strong> to {@code outputStream.write(byte[])}
     * — neither the Rust side nor the JVM ever holds the full body in
     * memory at once.
     *
     * <p>Returns the wire-format <strong>header bytes only</strong>
     * (length-prefixed JSON: status, headers, metadata).  The body
     * arrived via {@code outputStream} while the dispatch was in
     * flight.
     *
     * <p>Failure modes that happen <strong>before</strong> any body byte
     * was written (malformed wire, panic in Rust, no app registered, a
     * deadline before the header, or a body error before the first chunk)
     * return a regular
     * {@code error_wire(...)} response (header + small plain-text body) and
     * the {@code outputStream} is <strong>not</strong> written to.  A
     * {@code 422}'s validation errors are hoisted into the returned header.
     *
     * @param wireRequest  length-prefixed binary wire request
     * @param outputStream sink for response body chunks
     * @return wire-format header bytes (body lives on the OutputStream)
     * @throws IOException when the response failed <strong>after</strong>
     *         body bytes were written (body-stream error, the
     *         {@code outputStream} throwing): the written body is truncated
     *         and the transport must be aborted
     */
    public static native byte[] dispatchStreaming(byte[] wireRequest, OutputStream outputStream)
            throws IOException;

    /**
     * <strong>Bidirectional streaming</strong> binary wire-format JNI
     * dispatch — both request body (from {@code inputStream}) and
     * response body (to {@code outputStream}) are processed
     * chunk-by-chunk.  Neither side materialises the full body in
     * memory, so a 1 GiB upload paired with a 1 GiB download runs in
     * roughly {@code O(chunk_size)} RAM.
     *
     * <p>Wire envelope contract:
     * <ul>
     *   <li>{@code wireRequestHeader} is a wire-format request
     *       <strong>without a body</strong> — just the 4-byte length
     *       prefix + JSON header (method, path, query, headers).
     *       Use {@link #encodeRequest(String, String, String, java.util.Map, byte[])}
     *       with an empty {@code body} array.</li>
     *   <li>The request body bytes flow through {@code inputStream}
     *       — Rust calls {@code inputStream.read(byte[])} repeatedly
     *       (256 KiB at a time by default; see
     *       {@code vespera.streaming.chunkBytes}) until EOF.</li>
     *   <li>The response body bytes flow through {@code outputStream}
     *       — Rust calls {@code outputStream.write(byte[])} for each
     *       axum body frame.</li>
     * </ul>
     *
     * <p>Returns the wire-format <strong>header bytes only</strong>
     * (status, headers, metadata).  Decode with
     * {@link #decodeResponse(byte[])} to read the status and headers
     * — the body has already been written to {@code outputStream}.
     *
     * <p>Failure modes before any response byte was written (malformed
     * wire, panic in Rust, no app registered, request body over the
     * streaming cap) return a regular {@code error_wire(...)} response
     * (header + small plain-text body) and {@code outputStream} is not
     * written to.
     *
     * @param wireRequestHeader length-prefixed binary wire request
     *                          header (no body)
     * @param inputStream       source for request body chunks
     * @param outputStream      sink for response body chunks
     * @return wire-format header bytes (body lives on the
     *         {@code outputStream})
     * @throws IOException when the response failed after body bytes were
     *         written to {@code outputStream} (truncated response)
     */
    public static native byte[] dispatchFullStreaming(
            byte[] wireRequestHeader,
            InputStream inputStream,
            OutputStream outputStream) throws IOException;

    /**
     * Convenience encoder for the bidirectional streaming variant —
     * produces a wire-format header with an empty body, suitable for
     * passing to {@link #dispatchFullStreaming(byte[], InputStream, OutputStream)}.
     *
     * @param method  HTTP method (uppercase)
     * @param path    URL path
     * @param query   raw query string (may be {@code null})
     * @param headers request headers
     * @return wire bytes with the JSON header and no body
     */
    public static byte[] encodeRequestHeader(
            String method,
            String path,
            String query,
            java.util.Map<String, String> headers) {
        return encodeRequestHeader(null, method, path, query, headers);
    }

    public static byte[] encodeRequestHeader(
            String method,
            String path,
            String query,
            HeaderSource headers) {
        return encodeRequestHeader(null, method, path, query, headers);
    }

    /**
     * Same as {@link #encodeRequestHeader(String, String, String, java.util.Map)}
     * but with an explicit app name for multi-app routing.  See
     * {@link #encodeRequest(String, String, String, String, java.util.Map, byte[])}
     * for app name semantics.
     */
    public static byte[] encodeRequestHeader(
            String appName,
            String method,
            String path,
            String query,
            java.util.Map<String, String> headers) {
        return encodeRequest(
                appName,
                Objects.requireNonNull(method, "method"),
                Objects.requireNonNull(path, "path"),
                query,
                headers != null ? headers : java.util.Map.of(),
                VesperaWireCodec.EMPTY_BODY);
    }

    public static byte[] encodeRequestHeader(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers) {
        return encodeRequest(
                appName,
                Objects.requireNonNull(method, "method"),
                Objects.requireNonNull(path, "path"),
                query,
                headers,
                VesperaWireCodec.EMPTY_BODY);
    }

    /**
     * Variant of {@link #dispatchStreaming(byte[], OutputStream)} that
     * emits the wire-format response header via {@code headerConsumer}
     * <strong>before</strong> the first body byte reaches
     * {@code outputStream}.
     *
     * <p>This is the variant Spring {@link jakarta.servlet.http.HttpServletResponse}
     * controllers want: the header callback fires while the response
     * is still uncommitted, so the controller can call
     * {@code resp.setStatus(...)} / {@code resp.setHeader(...)} from
     * inside {@code headerConsumer.accept(byte[])}.
     *
     * <p>The {@code headerConsumer} is invoked <strong>exactly once</strong>
     * on every code path (success or error); the bytes are a normal
     * wire-format header (length-prefixed JSON).  Use
     * {@link #decodeResponse(byte[])} to extract status / headers /
     * metadata from those bytes.
     * A failure after the header throws the undeclared {@link VesperaStreamAbortedException}.
     */
    public static native void dispatchStreamingWithHeader(
            byte[] wireRequest,
            Consumer<byte[]> headerConsumer,
            OutputStream outputStream);

    /**
     * Variant of {@link #dispatchFullStreaming(byte[], InputStream, OutputStream)}
     * with the same header-callback contract as
     * {@link #dispatchStreamingWithHeader}.  Bidirectional streaming
     * + ability to commit Spring response status/headers before the
     * first body byte (post-header failures: {@link VesperaStreamAbortedException}).
     */
    public static native void dispatchFullStreamingWithHeader(
            byte[] wireRequestHeader,
            Consumer<byte[]> headerConsumer,
            InputStream inputStream,
            OutputStream outputStream);

    /**
     * Thrown by {@link #dispatchDirectPooled(byte[], boolean)} when the
     * response exceeds the out-buffer capacity and the caller disallowed
     * automatic retry (unsafe requests).  Carries the exact
     * buffer size needed for a successful retry.
     *
     * <p><strong>Retrying re-runs the dispatch</strong> — the Rust
     * handler executes again.  Only retry safe requests
     * (GET/HEAD/OPTIONS) automatically; for unsafe methods the caller
     * must decide.
     */
    public static class BufferTooSmallException extends RuntimeException {
        private final int requiredSize;

        public BufferTooSmallException(int requiredSize) {
            super("response requires a " + requiredSize
                    + "-byte direct out buffer; retry would re-run the dispatch");
            this.requiredSize = requiredSize;
        }

        BufferTooSmallException(int requiredSize, String message) {
            super(message);
            this.requiredSize = requiredSize;
        }

        /** Exact out-buffer capacity needed for a successful retry. */
        public int requiredSize() {
            return requiredSize;
        }
    }

    /**
     * Raw native entry — validated by {@link #dispatchDirect(ByteBuffer,
     * int, ByteBuffer)}; never call this directly.
     */
    private static native int dispatchDirect0(ByteBuffer in, int inLen, ByteBuffer out);

    /**
     * <strong>Direct-buffer</strong> synchronous dispatch — eliminates
     * both JNI region copies ({@code byte[]} ↔ native) and the per-call
     * Java heap array allocations of {@link #dispatchBytes(byte[])}.
     *
     * <p><strong>Contract</strong> (position/limit are IGNORED — the
     * explicit {@code inLen} parameter is authoritative):
     * <ul>
     *   <li>{@code in} and {@code out} MUST be <em>direct</em> buffers;
     *       heap buffers are rejected here, before crossing JNI.</li>
     *   <li>The wire request is read from absolute offsets
     *       {@code in[0..inLen]}.</li>
     *   <li>Return {@code >= 0}: a complete wire response occupies
     *       {@code out[0..n]}.</li>
     *   <li>Return {@code < 0}: {@code -(requiredSize)} — the response
     *       did not fit.  {@code out} contents are <em>undefined</em>
     *       (the response streams directly into the buffer, so a
     *       prefix may have been written).  {@code requiredSize} is
     *       exact; retrying re-runs the dispatch (see
     *       {@link BufferTooSmallException}).</li>
     *   <li>{@code Integer.MIN_VALUE}: response exceeds 2 GiB and is
     *       unrepresentable in this protocol.</li>
     * </ul>
     *
     * <p>The buffers are only accessed for the duration of this call;
     * they may be reused immediately after it returns.
     *
     * @param in    direct buffer holding the wire request at [0..inLen)
     * @param inLen number of valid request bytes in {@code in}
     * @param out   direct buffer that receives the wire response
     * @return bytes written, or the negative protocol codes above
     * @throws IllegalArgumentException if either buffer is not direct, read-only,
     *         {@code inLen} is negative, or exceeds {@code in.capacity()}
     */
    public static int dispatchDirect(ByteBuffer in, int inLen, ByteBuffer out) {
        validateDirectBuffers(in, inLen, out);
        return dispatchDirect0(in, inLen, out);
    }

    /** Caller-side checks shared by the direct-buffer natives. */
    static void validateDirectBuffers(ByteBuffer in, int inLen, ByteBuffer out) {
        Objects.requireNonNull(in, "in");
        Objects.requireNonNull(out, "out");
        if (!in.isDirect() || !out.isDirect()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires direct ByteBuffers (use ByteBuffer.allocateDirect)");
        }
        if (in.isReadOnly()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires a writable in ByteBuffer (got a read-only buffer)");
        }
        // SEC-2: the native side writes the wire response straight into
        // `out` via a `&mut [u8]`; a read-only direct buffer (e.g. a
        // read-only MappedByteBuffer) is backed by read-only pages, so
        // writing to it is undefined behavior / a process crash.  Reject
        // it here — the native code cannot recover from a write fault.
        if (out.isReadOnly()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires a writable out ByteBuffer (got a read-only buffer)");
        }
        if (inLen < 0 || inLen > in.capacity()) {
            throw new IllegalArgumentException(
                    "inLen " + inLen + " out of range for in.capacity() " + in.capacity());
        }
    }

    /**
     * Whether the calling thread is a virtual thread (Java 21+); always
     * {@code false} on the Java 17 baseline.  Delegates to
     * {@link VesperaDirectBufferPool#currentThreadIsVirtual()} — used by
     * {@link SmartDispatchModeResolver} to keep pooled direct-buffer work
     * off virtual threads.
     */
    static boolean currentThreadIsVirtual() {
        return VesperaDirectBufferPool.currentThreadIsVirtual();
    }

    /**
     * Pooled convenience around {@link #dispatchDirect(ByteBuffer, int,
     * ByteBuffer)} using per-thread reusable direct buffers (64 KiB
     * initial, doubling up to {@code vespera.direct.maxBufferBytes},
     * default 4 MiB).
     *
     * <p>Returns a <strong>read-only view</strong> of the thread-local
     * response buffer covering exactly the wire response bytes.  The
     * view is valid only until the next {@code dispatchDirect*} call on
     * the same thread — consume (or copy) it before dispatching again.
     *
     * <p><strong>Virtual thread (Project Loom) limitation:</strong> The
     * per-thread buffer pool is backed by {@link ThreadLocal}, which
     * binds to the <em>virtual thread</em> (not the carrier thread) in
     * Java 21+ semantics.  In a virtual-thread-per-request server, each
     * virtual thread allocates a fresh direct buffer and loses all
     * pooling benefit; direct memory accumulates until the virtual thread
     * is garbage-collected.  {@link VesperaDirectBufferPool} detects this
     * and routes virtual threads to the GC-managed heap
     * {@link #dispatchBytes(byte[])} path.
     *
     * <p>Fallback / overflow policy:
     * <ul>
     *   <li>Request larger than the cap → falls back to
     *       {@link #dispatchBytes(byte[])} (safe: no dispatch has run
     *       yet) and wraps the result.</li>
     *   <li>Response overflow with {@code retryOnOverflow == true} →
     *       grows the out buffer (or falls back to {@code dispatchBytes}
     *       beyond the cap) and dispatches again.  <strong>The handler
     *       runs twice</strong> — only pass {@code true} for safe
     *       requests.</li>
     *   <li>Response overflow with {@code retryOnOverflow == false} →
     *       throws {@link BufferTooSmallException}.</li>
     * </ul>
     *
     * @param wireRequest      length-prefixed binary wire request
     * @param retryOnOverflow  whether a response overflow may re-run the
     *                         dispatch (safe requests only)
     * @return read-only buffer view of the wire response, positioned at
     *         0 with {@code limit()} = response length
     */
    public static ByteBuffer dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow) {
        return VesperaDirectBufferPool.dispatchDirectPooled(wireRequest, retryOnOverflow);
    }

    /**
     * Encode-and-dispatch convenience that skips the intermediate
     * wire-sized {@code byte[]} entirely: the wire request is encoded
     * <strong>straight into the pooled direct in-buffer</strong>, so the
     * body bytes are copied heap→direct exactly once.  Same pooling,
     * fallback, overflow, and view-validity semantics as
     * {@link #dispatchDirectPooled(byte[], boolean)}.
     *
     * @param appName target app name (may be {@code null} for default)
     * @param method  HTTP method (uppercase)
     * @param path    URL path
     * @param query   raw query string (may be {@code null})
     * @param headers request headers
     * @param body    request body bytes (may be empty or {@code null})
     * @param retryOnOverflow whether a response overflow may re-run the
     *                        dispatch (safe requests only)
     * @return read-only buffer view of the wire response, valid until
     *         the next {@code dispatchDirect*} call on this thread
     */
    public static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body,
            boolean retryOnOverflow) {
        requireRequestInputs(method, path, headers);
        return VesperaDirectBufferPool.dispatchDirectPooled(
                appName, method, path, query, headers, body, retryOnOverflow);
    }

    public static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
            boolean retryOnOverflow) {
        requireRequestInputs(method, path);
        return VesperaDirectBufferPool.dispatchDirectPooled(
                appName, method, path, query, headers, body, retryOnOverflow);
    }

    static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
            boolean retryOnOverflow,
            boolean currentThreadIsVirtual) {
        requireRequestInputs(method, path);
        return VesperaDirectBufferPool.dispatchDirectPooled(
                appName, method, path, query, headers, body, retryOnOverflow, currentThreadIsVirtual,
                currentThreadIsVirtual ? ParkingDispatch::park : VesperaBridge::dispatchBytes, true);
    }

    /**
     * Encode a request <strong>directly into</strong> {@code target}
     * starting at position 0 — no intermediate wire-sized {@code byte[]}.
     *
     * <p>On success the wire bytes occupy {@code target[0..returned]}
     * and {@code target}'s position is left at the end of the written
     * region.  If {@code target} is too small, returns
     * {@code -(requiredSize)} and writes nothing.  This is an
     * <em>encoding-side</em> size signal: no dispatch has happened, so
     * growing the buffer and retrying is always safe (unlike the
     * response-overflow retry, which re-runs the handler).
     *
     * @param appName target app name (may be {@code null} for default)
     * @param method  HTTP method (uppercase)
     * @param path    URL path
     * @param query   raw query string (may be {@code null})
     * @param headers request headers
     * @param body    request body bytes (may be empty or {@code null})
     * @param target  destination buffer (any kind; for the JNI direct
     *                path use {@code ByteBuffer.allocateDirect})
     * @return total bytes written ({@code >= 4}), or {@code -(required)}
     */
    public static int encodeRequestInto(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body,
            ByteBuffer target) {
        Objects.requireNonNull(target, "target");
        requireRequestInputs(method, path, headers);
        return VesperaWireCodec.encodeRequestInto(appName, method, path, query, headers, body, target);
    }

    public static int encodeRequestInto(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
            ByteBuffer target) {
        Objects.requireNonNull(target, "target");
        requireRequestInputs(method, path);
        return VesperaWireCodec.encodeRequestInto(appName, method, path, query, headers, body, target);
    }

    /**
     * Encode a request into the binary wire format.
     *
     * @param method  HTTP method (uppercase: {@code GET}, {@code POST}, ...)
     * @param path    URL path including any path parameters
     * @param query   raw query string (empty / {@code null} if none)
     * @param headers request headers; lowercased keys are recommended
     * @param body    request body bytes (may be empty or {@code null})
     * @return length-prefixed wire bytes ready for {@link #dispatchBytes}
     */
    public static byte[] encodeRequest(
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body) {
        requireRequestInputs(method, path, headers);
        return VesperaWireCodec.encodeRequest(null, method, path, query, headers, body);
    }

    public static byte[] encodeRequest(
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body) {
        requireRequestInputs(method, path);
        return VesperaWireCodec.encodeRequest(null, method, path, query, headers, body);
    }

    /**
     * Encode a request into the binary wire format with an explicit
     * app name for multi-app routing.
     *
     * <p>When {@code appName} is {@code null}, empty, or blank, the
     * request is routed to the <strong>default</strong> app
     * (registered via the Rust {@code register_app} API).  Otherwise
     * the wire header carries {@code "app": "<appName>"} and the
     * request is routed to the named app (registered via
     * {@code register_app_named}).
     *
     * @param appName target app name (may be {@code null} for default)
     * @param method  HTTP method (uppercase: {@code GET}, {@code POST}, ...)
     * @param path    URL path including any path parameters
     * @param query   raw query string (empty / {@code null} if none)
     * @param headers request headers; lowercased keys are recommended
     * @param body    request body bytes (may be empty or {@code null})
     * @return length-prefixed wire bytes ready for any dispatch* method
     */
    public static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body) {
        requireRequestInputs(method, path, headers);
        return VesperaWireCodec.encodeRequest(appName, method, path, query, headers, body);
    }

    public static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body) {
        requireRequestInputs(method, path);
        return VesperaWireCodec.encodeRequest(appName, method, path, query, headers, body);
    }

    private static void requireRequestInputs(
            String method, String path, Map<String, String> headers) {
        requireRequestInputs(method, path);
        if (headers != null) {
            for (Map.Entry<String, String> header : headers.entrySet()) {
                Objects.requireNonNull(header.getKey(), "header key");
                Objects.requireNonNull(header.getValue(), "header value");
            }
        }
    }

    private static void requireRequestInputs(String method, String path) {
        Objects.requireNonNull(method, "method");
        Objects.requireNonNull(path, "path");
        if (path.indexOf('?') >= 0) {
            throw new IllegalArgumentException(
                    "path must not contain '?' — pass the raw query string via the query parameter");
        }
    }

    /**
     * Decode a wire-format response.
     *
     * @throws IllegalArgumentException if the wire bytes are malformed
     */
    public static DecodedResponse decodeResponse(byte[] wire) {
        return VesperaWireCodec.decodeResponse(wire);
    }

    private VesperaBridge() {}
}
