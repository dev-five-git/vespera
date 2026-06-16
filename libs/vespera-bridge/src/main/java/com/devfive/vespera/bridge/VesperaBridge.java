package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.ref.SoftReference;
import java.util.Objects;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.function.Consumer;

/**
 * JNI bridge to any Rust cdylib built with vespera's JNI feature.
 *
 * <p><strong>Wire format</strong> — both request and response use the
 * same layout:
 * <pre>
 *   bytes 0..4    : u32 BE = header_json byte length N
 *   bytes 4..4+N  : UTF-8 JSON header
 *                     (request)  { "v":1, "method", "path",
 *                                  "query"?, "headers"? }
 *                     (response) { "v":1, "status", "headers",
 *                                  "metadata" }
 *   bytes 4+N..   : raw body bytes (UTF-8 text or binary —
 *                   no encoding applied)
 * </pre>
 *
 * <p><strong>Usage</strong> — single line in your Spring Boot app:
 * <pre>{@code
 * VesperaBridge.init("rust_jni_demo");
 * }</pre>
 *
 * <p>The proxy controller ({@link VesperaProxyController}) is
 * auto-configured by Spring's component scan when this JAR is on the
 * classpath.
 */
public class VesperaBridge {

    /** Lowercase hex digits for the JSON C0 control-character escapes. */
    private static final byte[] HEX = {
        '0', '1', '2', '3', '4', '5', '6', '7',
        '8', '9', 'a', 'b', 'c', 'd', 'e', 'f'
    };
    private static final int WIRE_VERSION = 1;
    /** Shared empty request body — avoids a {@code new byte[0]} per call. */
    private static final byte[] EMPTY_BODY = new byte[0];
    /**
     * Per-thread reusable byte buffer for {@link #fillHeaderJson}.
     * Reset (size cleared, capacity preserved) per call and filled
     * byte-direct — no per-call encoder object.  Virtual-thread caveat
     * as {@link #DIRECT_POOL}: each vthread gets its own ~256 B buffer
     * in Java 21+ and loses pooling until GC.
     */
    private static final ThreadLocal<ExposedByteArrayOutputStream> HEADER_BUF =
            ThreadLocal.withInitial(() -> new ExposedByteArrayOutputStream(256));

    /**
     * {@link ByteArrayOutputStream} that exposes its backing array so the
     * serialized header is copied straight into the wire (heap array or
     * direct buffer) without {@link ByteArrayOutputStream#toByteArray()}
     * first materialising a second, exact-sized copy per request.
     *
     * <p>Callers MUST read only {@code [0, size())}: the backing array is
     * usually larger than the content (grow-by-doubling) and is reused
     * across calls on the same thread, so the bytes must be consumed
     * before the next {@link #fillHeaderJson} on that thread.
     */
    private static final class ExposedByteArrayOutputStream extends ByteArrayOutputStream {
        ExposedByteArrayOutputStream(int size) {
            super(size);
        }

        /** Backing buffer; valid content is {@code [0, size())} only. */
        byte[] backingArray() {
            return buf;
        }

        /**
         * Append one byte WITHOUT the inherited {@code synchronized} —
         * {@link #HEADER_BUF} is thread-local, so the monitor is pure
         * overhead on this single-threaded encode hot path.  Grows the
         * backing array by doubling, mirroring {@link ByteArrayOutputStream}.
         */
        void put(int b) {
            if (count == buf.length) {
                buf = java.util.Arrays.copyOf(buf, buf.length << 1);
            }
            buf[count++] = (byte) b;
        }

        /**
         * Append the bytes of an ASCII literal (caller guarantees every
         * char is {@code < 0x80}) — used for the fixed JSON structure
         * (keys, braces, colons).  Non-synchronized, single bulk reserve.
         */
        void putAscii(String lit) {
            int n = lit.length();
            if (count + n > buf.length) {
                int cap = buf.length;
                while (cap < count + n) {
                    cap <<= 1;
                }
                buf = java.util.Arrays.copyOf(buf, cap);
            }
            for (int i = 0; i < n; i++) {
                buf[count++] = (byte) lit.charAt(i);
            }
        }
    }

    private static volatile boolean loaded = false;

    private static volatile Integer pendingChunkBytes = null;
    private static volatile Integer pendingChannelCapacity = null;

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
            body = body.slice().asReadOnlyBuffer();
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
     * <p>Streaming configuration is seeded from system properties
     * <strong>before the first dispatch</strong> (values fixed for
     * the process lifetime once read):
     * <ul>
     *   <li>{@code vespera.streaming.chunkBytes} — per-chunk buffer
     *       size for streaming dispatches (default 256 KiB, clamped to
     *       4 KiB – 8 MiB on the Rust side)</li>
     *   <li>{@code vespera.streaming.channelCapacity} — bound of the
     *       bidirectional request-body channel in slots (default 16,
     *       clamped to 1 – 1024)</li>
     *   <li>{@code vespera.runtime.workerThreads} — worker threads of
     *       the shared Tokio runtime (default: number of logical
     *       CPUs, clamped to 1 – 1024)</li>
     * </ul>
     * The {@code VESPERA_STREAMING_CHUNK_BYTES} /
     * {@code VESPERA_STREAMING_CHANNEL_CAPACITY} /
     * {@code VESPERA_RUNTIME_WORKERS} environment variables apply
     * when no system property is set.
     *
     * @param libraryName Cargo crate name (e.g. {@code "rust_jni_demo"})
     */
    public static synchronized void init(String libraryName) {
        if (loaded) return;
        try {
            loadBundled(libraryName);
        } catch (UnsatisfiedLinkError e) {
            System.loadLibrary(libraryName);
        }
        // Apply pending streaming config (set via configureStreaming before init).
        // Pending values beat system properties (Rust-side setter > env > default).
        try {
            int chunkBytes = pendingChunkBytes != null
                    ? pendingChunkBytes
                    : Integer.getInteger("vespera.streaming.chunkBytes", 0);
            int channelCapacity = pendingChannelCapacity != null
                    ? pendingChannelCapacity
                    : Integer.getInteger("vespera.streaming.channelCapacity", 0);
            configureStreaming0(chunkBytes, channelCapacity);
        } catch (UnsatisfiedLinkError olderNativeLibrary) {
            // Pre-0.2 native libraries don't export configureStreaming0.
            // Streaming config then falls back to env vars / defaults —
            // never block init over an optional tuning hook.
        }
        try {
            configureRuntime0(Integer.getInteger("vespera.runtime.workerThreads", 0));
        } catch (UnsatisfiedLinkError olderNativeLibrary) {
            // Same guard as above — older native libraries fall back to
            // the VESPERA_RUNTIME_WORKERS env var / Tokio's default.
        }
        loaded = true;
    }

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
        if (loaded) {
            // Native library already loaded — apply immediately.
            configureStreaming0(chunkBytes, channelCapacity);
        } else {
            // Native library not yet loaded — store pending values.
            // These will be applied in init() before any dispatch.
            pendingChunkBytes = chunkBytes;
            pendingChannelCapacity = channelCapacity;
        }
    }

    /**
     * Seed the Rust-side streaming configuration.  Values {@code <= 0}
     * leave the corresponding setting untouched (environment variable
     * or built-in default applies).  Calls after the configuration is
     * fixed are silently ignored.
     */
    private static native void configureStreaming0(int chunkBytes, int channelCapacity);

    /**
     * Seed the shared Tokio runtime's worker thread count (system
     * property {@code vespera.runtime.workerThreads}, env fallback
     * {@code VESPERA_RUNTIME_WORKERS}; clamped to 1–1024 on the Rust
     * side).  Defaults to Tokio's heuristic (number of logical CPUs)
     * — cap it when the JVM's own thread pools compete for the same
     * cores.  Values {@code <= 0} leave the setting untouched; calls
     * after the runtime started are silently ignored.
     */
    private static native void configureRuntime0(int workerThreads);

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
     * <p>Cancellation is not propagated to the Rust task in this
     * release: {@code future.cancel(true)} will mark the future as
     * cancelled on the Java side, but the in-flight Rust dispatch
     * continues to completion (and its result is discarded).
     *
     * @param future        the future to complete with the wire response
     * @param wireRequest   length-prefixed binary wire request
     */
    public static native void dispatchAsync(
            CompletableFuture<byte[]> future, byte[] wireRequest);

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
     * <p>Failure modes (malformed wire, panic in Rust, no app
     * registered) return a regular {@code error_wire(...)} response
     * (header + small plain-text body) and the {@code outputStream}
     * is <strong>not</strong> written to.  Callers can detect a
     * streaming error by checking whether the returned bytes carry a
     * non-empty body via {@link #decodeResponse(byte[])}.
     *
     * @param wireRequest  length-prefixed binary wire request
     * @param outputStream sink for response body chunks
     * @return wire-format header bytes (body lives on the OutputStream)
     */
    public static native byte[] dispatchStreaming(byte[] wireRequest, OutputStream outputStream);

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
     * <p>Failure modes (malformed wire, panic in Rust, no app
     * registered) return a regular {@code error_wire(...)} response
     * (header + small plain-text body) and <strong>neither</strong>
     * stream is touched.
     *
     * @param wireRequestHeader length-prefixed binary wire request
     *                          header (no body)
     * @param inputStream       source for request body chunks
     * @param outputStream      sink for response body chunks
     * @return wire-format header bytes (body lives on the
     *         {@code outputStream})
     */
    public static native byte[] dispatchFullStreaming(
            byte[] wireRequestHeader,
            InputStream inputStream,
            OutputStream outputStream);

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
                EMPTY_BODY);
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
     * first body byte.
     */
    public static native void dispatchFullStreamingWithHeader(
            byte[] wireRequestHeader,
            Consumer<byte[]> headerConsumer,
            InputStream inputStream,
            OutputStream outputStream);

    /**
     * Thrown by {@link #dispatchDirectPooled(byte[], boolean)} when the
     * response exceeds the out-buffer capacity and the caller disallowed
     * automatic retry (non-idempotent requests).  Carries the exact
     * buffer size needed for a successful retry.
     *
     * <p><strong>Retrying re-runs the dispatch</strong> — the Rust
     * handler executes again.  Only retry idempotent requests
     * (GET/HEAD/PUT/DELETE) automatically; for POST/PATCH the caller
     * must decide.
     */
    public static final class BufferTooSmallException extends RuntimeException {
        private final int requiredSize;

        public BufferTooSmallException(int requiredSize) {
            super("response requires a " + requiredSize
                    + "-byte direct out buffer; retry would re-run the dispatch");
            this.requiredSize = requiredSize;
        }

        /** Exact out-buffer capacity needed for a successful retry. */
        public int requiredSize() {
            return requiredSize;
        }
    }

    /** Initial per-thread direct buffer capacity (64 KiB). */
    private static final int DIRECT_INITIAL_CAPACITY = 64 * 1024;

    /**
     * Maximum per-thread direct buffer capacity (default 4 MiB,
     * overridable via the {@code vespera.direct.maxBufferBytes} system
     * property).  Payloads beyond the cap fall back to
     * {@link #dispatchBytes(byte[])}.
     */
    private static final int DIRECT_MAX_CAPACITY = Integer.getInteger(
            "vespera.direct.maxBufferBytes", 4 * 1024 * 1024);

    /**
     * Per-thread <strong>hard retention cap</strong> for the pooled
     * direct buffers (system property
     * {@code vespera.direct.maxRetainedBytes}, default 2 MiB; clamped
     * to [{@link #DIRECT_INITIAL_CAPACITY}, {@link #DIRECT_MAX_CAPACITY}]).
     *
     * <p>A buffer that a large dispatch grew beyond this cap is shrunk
     * back to {@link #DIRECT_INITIAL_CAPACITY} at the start of the next
     * dispatch on the same thread, so a single big response cannot pin
     * off-heap memory for the thread's whole lifetime.  Transient growth
     * up to {@link #DIRECT_MAX_CAPACITY} for an individual request is
     * still allowed — only steady-state retention is capped.
     *
     * <p><strong>Default raised from 256 KiB to 2 MiB (measured 2026-06).</strong>
     * Bodyless requests (the common GET) always take DIRECT regardless of
     * response size, so when the cap sat below the response size every such
     * dispatch shrank the buffer, overflowed, regrew, and <em>re-ran the
     * handler</em> — measured 6&ndash;8&times; slower than streaming for
     * 256 KiB&ndash;1.5 MiB responses (e.g. a {@code GET} download).  At
     * 2 MiB DIRECT instead beats streaming by 1.7&ndash;2.7&times; across
     * that range.  The cost is self-targeting: only threads that actually
     * handle large responses retain more (small-response threads keep the
     * 64 KiB baseline), and the pool is {@link SoftReference}-backed so the
     * JVM reclaims it under memory pressure.  Memory-sensitive deployments
     * dial it back via {@code vespera.direct.maxRetainedBytes}.
     */
    private static final int DIRECT_RETAIN_CAPACITY = Math.max(
            DIRECT_INITIAL_CAPACITY,
            Math.min(DIRECT_MAX_CAPACITY,
                    Integer.getInteger("vespera.direct.maxRetainedBytes", 2 * 1024 * 1024)));

    /**
     * Index 0 = request buffer, index 1 = response buffer.
     *
     * <p>Held through a {@link SoftReference} so the JVM can reclaim the
     * off-heap direct buffers under memory pressure — the
     * {@code DirectByteBuffer} Cleaner frees the native memory once the
     * soft reference is cleared — instead of pinning up to {@code 2 ×}
     * {@link #DIRECT_MAX_CAPACITY} per thread for the whole thread
     * lifetime.  Under normal load the soft reference survives, so the
     * pooling benefit is preserved; see {@link #directPool()} for the
     * resolve + retention-cap logic.
     *
     * <p><strong>Virtual thread limitation:</strong> {@link ThreadLocal}
     * binds to the virtual thread (not the carrier) in Java 21+.  Each
     * virtual thread gets its own pool, losing the pooling benefit in
     * virtual-thread-per-request servers.  See
     * {@link #dispatchDirectPooled(byte[], boolean)} for mitigation.
     */
    private static final ThreadLocal<SoftReference<ByteBuffer[]>> DIRECT_POOL =
            new ThreadLocal<>();

    /**
     * Resolve the calling thread's pooled direct buffers, (re)allocating
     * a baseline pair when the {@link SoftReference} has been cleared
     * under memory pressure, and shrinking any buffer a prior large
     * dispatch grew past {@link #DIRECT_RETAIN_CAPACITY} back to the
     * baseline.
     *
     * <p>Shrinking here — at the <em>start</em> of a dispatch, before any
     * request bytes are written into the pool — is safe with respect to
     * the "view valid until the next dispatch" contract of
     * {@link #dispatchDirectPooled(byte[], boolean)}: the previous
     * response view's validity window has already ended by the time the
     * next dispatch begins.
     */
    private static ByteBuffer[] directPool() {
        SoftReference<ByteBuffer[]> ref = DIRECT_POOL.get();
        ByteBuffer[] pool = ref == null ? null : ref.get();
        if (pool == null) {
            pool = new ByteBuffer[] {
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY),
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY)};
            DIRECT_POOL.set(new SoftReference<>(pool));
            return pool;
        }
        if (pool[0].capacity() > DIRECT_RETAIN_CAPACITY) {
            pool[0] = ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY);
        }
        if (pool[1].capacity() > DIRECT_RETAIN_CAPACITY) {
            pool[1] = ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY);
        }
        return pool;
    }

    /**
     * Handle to {@code Thread.isVirtual()} (final API since Java 21),
     * resolved reflectively so this library still compiles and runs on
     * the Java 17 baseline.  {@code null} on pre-21 runtimes, where no
     * thread is ever virtual.
     */
    private static final java.lang.invoke.MethodHandle IS_VIRTUAL = resolveIsVirtual();

    private static java.lang.invoke.MethodHandle resolveIsVirtual() {
        try {
            return java.lang.invoke.MethodHandles.lookup()
                    .findVirtual(Thread.class, "isVirtual",
                            java.lang.invoke.MethodType.methodType(boolean.class));
        } catch (ReflectiveOperationException pre21Runtime) {
            return null;
        }
    }

    /**
     * Whether the calling thread is a virtual thread (Java 21+); always
     * {@code false} on the Java 17 baseline runtime.
     *
     * <p>The pooled direct-buffer fast path is backed by
     * {@link ThreadLocal}, which binds to the <em>virtual</em> thread
     * (not its carrier) in Java 21+ — so on a virtual-thread-per-request
     * server every dispatch would allocate a fresh direct buffer and
     * accumulate off-heap memory until GC.  {@link #dispatchDirectPooled}
     * detects this and routes virtual threads to the GC-managed heap
     * {@link #dispatchBytes(byte[])} path instead, automating the
     * mitigation the docs previously left to manual configuration.
     */
    static boolean currentThreadIsVirtual() {
        if (IS_VIRTUAL == null) {
            return false;
        }
        try {
            return (boolean) IS_VIRTUAL.invokeExact(Thread.currentThread());
        } catch (Throwable ignoredFallBackToPooled) {
            return false;
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
     * @throws IllegalArgumentException if either buffer is not direct,
     *         {@code inLen} is negative, or exceeds {@code in.capacity()}
     */
    public static int dispatchDirect(ByteBuffer in, int inLen, ByteBuffer out) {
        Objects.requireNonNull(in, "in");
        Objects.requireNonNull(out, "out");
        if (!in.isDirect() || !out.isDirect()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires direct ByteBuffers (use ByteBuffer.allocateDirect)");
        }
        if (inLen < 0 || inLen > in.capacity()) {
            throw new IllegalArgumentException(
                    "inLen " + inLen + " out of range for in.capacity() " + in.capacity());
        }
        return dispatchDirect0(in, inLen, out);
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
     * is garbage-collected.  For virtual-thread deployments, prefer
     * {@link #dispatchBytes(byte[])}, {@link #dispatchStreaming}, or
     * {@link #dispatchFullStreaming}, or run dispatch on a bounded
     * platform-thread executor, or lower {@code vespera.direct.maxBufferBytes}.
     *
     * <p>Fallback / overflow policy:
     * <ul>
     *   <li>Request larger than the cap → falls back to
     *       {@link #dispatchBytes(byte[])} (safe: no dispatch has run
     *       yet) and wraps the result.</li>
     *   <li>Response overflow with {@code retryOnOverflow == true} →
     *       grows the out buffer (or falls back to {@code dispatchBytes}
     *       beyond the cap) and dispatches again.  <strong>The handler
     *       runs twice</strong> — only pass {@code true} for idempotent
     *       requests.</li>
     *   <li>Response overflow with {@code retryOnOverflow == false} →
     *       throws {@link BufferTooSmallException}.</li>
     * </ul>
     *
     * @param wireRequest      length-prefixed binary wire request
     * @param retryOnOverflow  whether a response overflow may re-run the
     *                         dispatch (idempotent requests only)
     * @return read-only buffer view of the wire response, positioned at
     *         0 with {@code limit()} = response length
     */
    public static ByteBuffer dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow) {
        Objects.requireNonNull(wireRequest, "wireRequest");
        if (currentThreadIsVirtual() || wireRequest.length > DIRECT_MAX_CAPACITY) {
            // Virtual thread: the per-thread direct buffer pool would
            // accumulate off-heap memory per vthread (ThreadLocal binds to
            // the vthread, not the carrier) — use the GC-managed heap path.
            // Oversized request (> cap): byte[] fallback is safe for any
            // method because no dispatch has run yet.
            return ByteBuffer.wrap(dispatchBytes(wireRequest)).asReadOnlyBuffer();
        }
        ByteBuffer[] pool = directPool();
        if (pool[0].capacity() < wireRequest.length) {
            pool[0] = ByteBuffer.allocateDirect(grownCapacity(wireRequest.length));
        }
        ByteBuffer in = pool[0];
        in.clear();
        in.put(wireRequest);

        return dispatchViaPool(pool, wireRequest.length, retryOnOverflow, () -> wireRequest);
    }

    /**
     * Encode-and-dispatch convenience that skips the intermediate
     * wire-sized {@code byte[]} entirely: the wire request is encoded
     * <strong>straight into the pooled direct in-buffer</strong> via
     * {@link #encodeRequestInto}, so the body bytes are copied
     * heap→direct exactly once (the {@code byte[]}-based overload
     * assembles a full wire array first and then copies it again).
     *
     * <p>Same pooling, fallback, overflow, and view-validity semantics
     * as {@link #dispatchDirectPooled(byte[], boolean)}.  Note the two
     * distinct retry concepts: <em>encoding</em> growth (request bigger
     * than the pooled buffer) happens before any dispatch and is always
     * safe; <em>response-overflow</em> retry re-runs the Rust handler
     * and is gated by {@code retryOnOverflow}.
     *
     * <p><strong>Virtual thread (Project Loom) limitation:</strong> The
     * per-thread buffer pool is backed by {@link ThreadLocal}, which
     * binds to the <em>virtual thread</em> (not the carrier thread) in
     * Java 21+ semantics.  In a virtual-thread-per-request server, each
     * virtual thread allocates a fresh direct buffer and loses all
     * pooling benefit; direct memory accumulates until the virtual thread
     * is garbage-collected.  For virtual-thread deployments, prefer
     * {@link #dispatchBytes(byte[])}, {@link #dispatchStreaming}, or
     * {@link #dispatchFullStreaming}, or run dispatch on a bounded
     * platform-thread executor, or lower {@code vespera.direct.maxBufferBytes}.
     *
     * @param appName target app name (may be {@code null} for default)
     * @param method  HTTP method (uppercase)
     * @param path    URL path
     * @param query   raw query string (may be {@code null})
     * @param headers request headers
     * @param body    request body bytes (may be empty or {@code null})
     * @param retryOnOverflow whether a response overflow may re-run the
     *                        dispatch (idempotent requests only)
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
        byte[] bodyBytes = body != null ? body : EMPTY_BODY;
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        int headerLen = hdr.size();
        int total = 4 + headerLen + bodyBytes.length;
        if (currentThreadIsVirtual() || total > DIRECT_MAX_CAPACITY) {
            // Virtual thread: avoid the per-vthread off-heap direct buffer
            // accumulation — use the GC-managed heap path.  Oversized
            // request (> cap): byte[] fallback is safe for any method
            // because no dispatch has run yet.  The reusable header buffer
            // is consumed here, before any other fillHeaderJson call.
            return ByteBuffer.wrap(
                    dispatchBytes(assembleWire(hdr.backingArray(), headerLen, bodyBytes)))
                    .asReadOnlyBuffer();
        }
        ByteBuffer[] pool = directPool();
        if (pool[0].capacity() < total) {
            pool[0] = ByteBuffer.allocateDirect(grownCapacity(total));
        }
        // Consume the reusable header buffer into the pooled direct buffer
        // now; dispatchViaPool's lazy wireFallback re-encodes from scratch
        // rather than capturing the buffer, so buffer reuse cannot corrupt
        // a deferred fallback.
        int written = assembleInto(hdr.backingArray(), headerLen, bodyBytes, pool[0]);
        if (written != total) {
            throw new IllegalStateException(
                    "assembleInto wrote " + written + ", expected " + total);
        }
        return dispatchViaPool(pool, total, retryOnOverflow,
                () -> encodeRequest(appName, method, path, query, headers, bodyBytes));
    }

    /**
     * Dispatch the request already prepared in the pooled in-buffer
     * ({@code pool[0][0..reqLen]}) and apply the response-overflow
     * policy.  {@code wireFallback} supplies the equivalent wire bytes
     * lazily — only materialised when a permitted retry exceeds the
     * pool cap and must take the {@code dispatchBytes} path.
     */
    private static ByteBuffer dispatchViaPool(
            ByteBuffer[] pool, int reqLen, boolean retryOnOverflow,
            java.util.function.Supplier<byte[]> wireFallback) {
        int n = dispatchDirect(pool[0], reqLen, pool[1]);
        if (n < 0 && n != Integer.MIN_VALUE) {
            int required = -n;
            if (!retryOnOverflow) {
                throw new BufferTooSmallException(required);
            }
            if (required > DIRECT_MAX_CAPACITY) {
                // Retry permitted; beyond the pool cap use the byte[] path.
                return ByteBuffer.wrap(dispatchBytes(wireFallback.get())).asReadOnlyBuffer();
            }
            pool[1] = ByteBuffer.allocateDirect(grownCapacity(required));
            n = dispatchDirect(pool[0], reqLen, pool[1]);
        }
        if (n < 0 && n != Integer.MIN_VALUE) {
            // A second overflow is legitimate: the retry re-ran the
            // handler, and a non-deterministic handler may produce a
            // larger response this time.  Surface the new exact size
            // instead of retrying unboundedly.
            throw new BufferTooSmallException(-n);
        }
        if (n < 0) {
            throw new IllegalStateException(
                    "dispatchDirect protocol violation: return code " + n + " after retry");
        }
        ByteBuffer view = pool[1].asReadOnlyBuffer();
        view.position(0).limit(n);
        return view;
    }

    /**
     * Encode a wire request <strong>directly into</strong> {@code target}
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
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        return assembleInto(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY, target);
    }

    /** Internal: write {@code [u32 BE len | headerJson[0..headerLen] | body]} at position 0. */
    private static int assembleInto(byte[] headerJson, int headerLen, byte[] body, ByteBuffer target) {
        int total = 4 + headerLen + body.length;
        if (target.capacity() < total) {
            return -total;
        }
        target.clear();
        target.order(ByteOrder.BIG_ENDIAN);
        target.putInt(headerLen);
        target.put(headerJson, 0, headerLen);
        if (body.length > 0) {
            target.put(body);
        }
        return total;
    }

    /** Internal: assemble a heap wire array from pre-serialised parts. */
    private static byte[] assembleWire(byte[] headerJson, int headerLen, byte[] body) {
        byte[] wire = new byte[4 + headerLen + body.length];
        // Write the u32 BE length prefix directly — avoids the
        // HeapByteBuffer wrapper object that
        // ByteBuffer.allocate(...).array() allocates per request; the
        // arraycopy intrinsics handle the header + body.  Byte-identical
        // to the prior ByteBuffer path.
        wire[0] = (byte) (headerLen >>> 24);
        wire[1] = (byte) (headerLen >>> 16);
        wire[2] = (byte) (headerLen >>> 8);
        wire[3] = (byte) headerLen;
        System.arraycopy(headerJson, 0, wire, 4, headerLen);
        System.arraycopy(body, 0, wire, 4 + headerLen, body.length);
        return wire;
    }

    /** Smallest power-of-two-ish growth ≥ {@code needed}, capped. */
    private static int grownCapacity(int needed) {
        int cap = DIRECT_INITIAL_CAPACITY;
        while (cap < needed) {
            cap = Math.min(cap * 2, DIRECT_MAX_CAPACITY);
            if (cap == DIRECT_MAX_CAPACITY) break;
        }
        return Math.max(cap, needed);
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
        return encodeRequest(null, method, path, query, headers, body);
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
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        return assembleWire(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY);
    }

    /**
     * Internal: serialise the wire request header JSON
     * <strong>byte-direct</strong> into the per-thread {@link #HEADER_BUF}
     * — no Jackson generator (and its per-call object + scratch buffer)
     * is allocated.  Emits the same shape and field order the prior
     * {@code JsonGenerator} path did ({@code v}, {@code method},
     * {@code path}, optional {@code query}/{@code headers}/{@code app}),
     * with the same omission rules.  String values are escaped + UTF-8
     * encoded by {@link #writeJsonString} using exactly the escape set
     * Jackson's {@code UTF8JsonGenerator} produced (the quote, the
     * backslash, and the C0 controls; {@code /} and non-ASCII pass
     * through), so the bytes stay valid JSON the Rust {@code serde_json}
     * side parses identically.
     */
    private static ExposedByteArrayOutputStream fillHeaderJson(String appName, String method,
            String path, String query, Map<String, String> headers) {
        ExposedByteArrayOutputStream buf = HEADER_BUF.get();
        buf.reset();
        // {"v":<WIRE_VERSION>, ...} — WIRE_VERSION is a single decimal digit.
        buf.putAscii("{\"v\":");
        buf.put('0' + WIRE_VERSION);
        buf.putAscii(",\"method\":");
        writeJsonString(buf, method);
        buf.putAscii(",\"path\":");
        writeJsonString(buf, path);
        if (query != null && !query.isEmpty()) {
            buf.putAscii(",\"query\":");
            writeJsonString(buf, query);
        }
        if (headers != null && !headers.isEmpty()) {
            buf.putAscii(",\"headers\":{");
            boolean first = true;
            for (Map.Entry<String, String> e : headers.entrySet()) {
                if (!first) {
                    buf.put(',');
                }
                first = false;
                writeJsonString(buf, e.getKey());
                buf.put(':');
                writeJsonString(buf, e.getValue());
            }
            buf.put('}');
        }
        if (appName != null && !appName.isBlank()) {
            buf.putAscii(",\"app\":");
            writeJsonString(buf, appName.trim());
        }
        buf.put('}');
        return buf;
    }

    /**
     * Append {@code s} as a quoted JSON string straight into {@code out}
     * as UTF-8, escaping only the JSON-mandatory characters — the quote,
     * the backslash, and the C0 controls (short {@code \b \t \n \f \r}
     * forms, four-hex escapes otherwise) — exactly the set the prior
     * Jackson {@code UTF8JsonGenerator} emitted (it does not escape
     * {@code /} or non-ASCII).  Single pass, no per-string {@code byte[]}:
     * printable ASCII is written verbatim, the rest UTF-8 encoded inline
     * (surrogate pairs become 4-byte sequences).
     */
    private static void writeJsonString(ExposedByteArrayOutputStream out, String s) {
        out.put('"');
        int n = s.length();
        for (int i = 0; i < n; i++) {
            char c = s.charAt(i);
            if (c >= 0x20 && c < 0x80) {
                if (c == '"' || c == '\\') {
                    out.put('\\');
                }
                out.put(c);
            } else if (c < 0x20) {
                switch (c) {
                    case '\b' -> {
                        out.put('\\');
                        out.put('b');
                    }
                    case '\t' -> {
                        out.put('\\');
                        out.put('t');
                    }
                    case '\n' -> {
                        out.put('\\');
                        out.put('n');
                    }
                    case '\f' -> {
                        out.put('\\');
                        out.put('f');
                    }
                    case '\r' -> {
                        out.put('\\');
                        out.put('r');
                    }
                    default -> {
                        out.put('\\');
                        out.put('u');
                        out.put('0');
                        out.put('0');
                        out.put(HEX[(c >> 4) & 0xF]);
                        out.put(HEX[c & 0xF]);
                    }
                }
            } else if (c < 0x800) {
                out.put(0xC0 | (c >> 6));
                out.put(0x80 | (c & 0x3F));
            } else if (Character.isHighSurrogate(c)
                    && i + 1 < n
                    && Character.isLowSurrogate(s.charAt(i + 1))) {
                int cp = Character.toCodePoint(c, s.charAt(++i));
                out.put(0xF0 | (cp >> 18));
                out.put(0x80 | ((cp >> 12) & 0x3F));
                out.put(0x80 | ((cp >> 6) & 0x3F));
                out.put(0x80 | (cp & 0x3F));
            } else {
                out.put(0xE0 | (c >> 12));
                out.put(0x80 | ((c >> 6) & 0x3F));
                out.put(0x80 | (c & 0x3F));
            }
        }
        out.put('"');
    }

    /**
     * Decode a wire-format response.
     *
     * @throws IllegalArgumentException if the wire bytes are malformed
     */
    public static DecodedResponse decodeResponse(byte[] wire) {
        if (wire == null || wire.length < 4) {
            throw new IllegalArgumentException(
                    "wire response too short: "
                            + (wire == null ? "null" : wire.length + " bytes"));
        }
        int headerLen = ((wire[0] & 0xFF) << 24) | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8) | (wire[3] & 0xFF);
        if (headerLen < 0 || (long) 4 + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen
                            + " overflows response (" + wire.length + " bytes)");
        }
        // Manual decode via the allocation-lean WireHeaderReader tokenizer
        // (the same parser the DIRECT / streaming header callbacks use)
        // instead of a Jackson JsonParser — drops the per-response parser +
        // IOContext allocation.  Output is shape-identical: status (default
        // 500), headers (String | List<String>), metadata (pre-sized),
        // validation_errors, and unknown fields (incl. "v") skipped.
        WireHeaderReader.Decoded d =
                WireHeaderReader.decode(ByteBuffer.wrap(wire), 4, headerLen);
        ByteBuffer body = ByteBuffer.wrap(wire, 4 + headerLen, wire.length - 4 - headerLen);
        return new DecodedResponse(
                d.status,
                d.headers == null ? Map.of() : d.headers,
                d.metadata,
                body,
                d.validationErrors);
    }

    private static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;

        try (InputStream in =
                VesperaBridge.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new UnsatisfiedLinkError("Not found in JAR: " + resourcePath);
            }
            String suffix = filename.substring(filename.lastIndexOf('.'));
            Path temp = Files.createTempFile("vespera-", suffix);
            temp.toFile().deleteOnExit();
            Files.copy(in, temp, StandardCopyOption.REPLACE_EXISTING);
            System.load(temp.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UnsatisfiedLinkError("Extract failed: " + e.getMessage());
        }
    }

    private static String detectOs() {
        String os = System.getProperty("os.name", "").toLowerCase();
        if (os.contains("win")) return "windows";
        if (os.contains("mac") || os.contains("darwin")) return "macos";
        return "linux";
    }

    private static String detectArch() {
        String arch = System.getProperty("os.arch", "").toLowerCase();
        if (arch.contains("amd64") || arch.contains("x86_64")) return "x86_64";
        if (arch.contains("aarch64") || arch.contains("arm64")) return "aarch64";
        return arch;
    }

    private static String mapLibraryName(String os, String name) {
        return switch (os) {
            case "windows" -> name + ".dll";
            case "macos" -> "lib" + name + ".dylib";
            default -> "lib" + name + ".so";
        };
    }

    private VesperaBridge() {}
}
