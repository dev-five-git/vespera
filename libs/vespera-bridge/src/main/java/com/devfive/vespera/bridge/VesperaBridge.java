package com.devfive.vespera.bridge;

import com.fasterxml.jackson.core.JsonFactory;
import com.fasterxml.jackson.core.JsonGenerator;
import com.fasterxml.jackson.core.JsonParser;
import com.fasterxml.jackson.core.JsonToken;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Objects;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.LinkedHashMap;
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

    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final JsonFactory JSON_FACTORY = MAPPER.getFactory();
    private static final int WIRE_VERSION = 1;
    /**
     * Per-thread reusable byte buffer for {@link #serializeHeaderJson}.
     * Reset (size cleared, capacity preserved) per call; only the
     * buffer is pooled — a fresh {@link JsonGenerator} is created per
     * call because generators bind to stream state.  Virtual-thread
     * caveat as {@link #DIRECT_POOL}: each vthread gets its own ~256 B
     * buffer in Java 21+ and loses pooling until GC.
     */
    private static final ThreadLocal<ByteArrayOutputStream> HEADER_BUF =
            ThreadLocal.withInitial(() -> new ByteArrayOutputStream(256));

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
                new byte[0]);
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
     * Index 0 = request buffer, index 1 = response buffer.
     *
     * <p><strong>Virtual thread limitation:</strong> {@link ThreadLocal}
     * binds to the virtual thread (not the carrier) in Java 21+.  Each
     * virtual thread gets its own pool, losing the pooling benefit in
     * virtual-thread-per-request servers.  See
     * {@link #dispatchDirectPooled(byte[], boolean)} for mitigation.
     */
    private static final ThreadLocal<ByteBuffer[]> DIRECT_POOL =
            ThreadLocal.withInitial(() -> new ByteBuffer[] {
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY),
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY)});

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
        if (wireRequest.length > DIRECT_MAX_CAPACITY) {
            // No dispatch has run yet — byte[] fallback is safe for any method.
            return ByteBuffer.wrap(dispatchBytes(wireRequest)).asReadOnlyBuffer();
        }
        ByteBuffer[] pool = DIRECT_POOL.get();
        if (pool[0].capacity() < wireRequest.length) {
            pool[0] = ByteBuffer.allocateDirect(grownCapacity(wireRequest.length));
        }
        ByteBuffer in = pool[0];
        in.clear();
        in.put(wireRequest);

        return dispatchViaPool(wireRequest.length, retryOnOverflow, () -> wireRequest);
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
        byte[] headerJson = serializeHeaderJson(appName, method, path, query, headers);
        byte[] bodyBytes = body != null ? body : new byte[0];
        int total = 4 + headerJson.length + bodyBytes.length;
        if (total > DIRECT_MAX_CAPACITY) {
            // No dispatch has run yet — byte[] fallback is safe for any method.
            return ByteBuffer.wrap(dispatchBytes(assembleWire(headerJson, bodyBytes)))
                    .asReadOnlyBuffer();
        }
        ByteBuffer[] pool = DIRECT_POOL.get();
        if (pool[0].capacity() < total) {
            pool[0] = ByteBuffer.allocateDirect(grownCapacity(total));
        }
        int written = encodeRequestInto(headerJson, bodyBytes, pool[0]);
        if (written != total) {
            throw new IllegalStateException(
                    "encodeRequestInto wrote " + written + ", expected " + total);
        }
        return dispatchViaPool(total, retryOnOverflow,
                () -> assembleWire(headerJson, bodyBytes));
    }

    /**
     * Dispatch the request already prepared in the pooled in-buffer
     * ({@code pool[0][0..reqLen]}) and apply the response-overflow
     * policy.  {@code wireFallback} supplies the equivalent wire bytes
     * lazily — only materialised when a permitted retry exceeds the
     * pool cap and must take the {@code dispatchBytes} path.
     */
    private static ByteBuffer dispatchViaPool(
            int reqLen, boolean retryOnOverflow, java.util.function.Supplier<byte[]> wireFallback) {
        ByteBuffer[] pool = DIRECT_POOL.get();
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
        byte[] headerJson = serializeHeaderJson(appName, method, path, query, headers);
        return encodeRequestInto(headerJson, body != null ? body : new byte[0], target);
    }

    /** Internal: write {@code [u32 BE len | headerJson | body]} at position 0. */
    private static int encodeRequestInto(byte[] headerJson, byte[] body, ByteBuffer target) {
        int total = 4 + headerJson.length + body.length;
        if (target.capacity() < total) {
            return -total;
        }
        target.clear();
        target.order(ByteOrder.BIG_ENDIAN);
        target.putInt(headerJson.length);
        target.put(headerJson);
        if (body.length > 0) {
            target.put(body);
        }
        return total;
    }

    /** Internal: assemble a heap wire array from pre-serialised parts. */
    private static byte[] assembleWire(byte[] headerJson, byte[] body) {
        ByteBuffer buf = ByteBuffer
                .allocate(4 + headerJson.length + body.length)
                .order(ByteOrder.BIG_ENDIAN);
        buf.putInt(headerJson.length);
        buf.put(headerJson);
        buf.put(body);
        return buf.array();
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
        byte[] headerJson = serializeHeaderJson(appName, method, path, query, headers);
        return assembleWire(headerJson, body != null ? body : new byte[0]);
    }

    /**
     * Internal: serialise the wire request header JSON via Jackson's
     * streaming {@link JsonGenerator} writing directly into the
     * per-thread {@link #HEADER_BUF}.  Byte-identical to the prior
     * {@code createObjectNode() + writeValueAsBytes()} path: same
     * field order ({@code v}, {@code method}, {@code path}, optional
     * {@code query}/{@code headers}/{@code app}), same omission rules,
     * same {@code UTF8JsonGenerator} emitter — the {@code ObjectNode}
     * tree and {@code writeValueAsBytes} scratch buffer go away.
     * (A 3-pass {@code StringBuilder} encoder was previously measured
     * <em>slower</em>, 656 vs 487 ns/op; the generator writes bytes
     * directly, so this rewrite keeps that win and drops the tree.)
     */
    private static byte[] serializeHeaderJson(String appName, String method,
            String path, String query, Map<String, String> headers) {
        ByteArrayOutputStream buf = HEADER_BUF.get();
        buf.reset();
        try (JsonGenerator gen = JSON_FACTORY.createGenerator(buf)) {
            gen.writeStartObject();
            gen.writeNumberField("v", WIRE_VERSION);
            gen.writeStringField("method", method);
            gen.writeStringField("path", path);
            if (query != null && !query.isEmpty()) {
                gen.writeStringField("query", query);
            }
            if (headers != null && !headers.isEmpty()) {
                gen.writeObjectFieldStart("headers");
                for (Map.Entry<String, String> e : headers.entrySet()) {
                    gen.writeStringField(e.getKey(), e.getValue());
                }
                gen.writeEndObject();
            }
            if (appName != null && !appName.isBlank()) {
                gen.writeStringField("app", appName.trim());
            }
            gen.writeEndObject();
        } catch (IOException e) {
            throw new IllegalStateException("encodeRequest serialisation failed", e);
        }
        return buf.toByteArray();
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
        ByteBuffer buf = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN);
        int headerLen = buf.getInt();
        if (headerLen < 0 || (long) 4 + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen
                            + " overflows response (" + wire.length + " bytes)");
        }
        // Streaming decode via JsonParser (no JsonNode tree); defaults match
        // the readTree path, unknown fields (incl. "v") are skipChildren'd.
        int status = 500;
        Map<String, Object> headers = new LinkedHashMap<>();
        Map<String, String> metadata = new LinkedHashMap<>();
        List<Map<String, Object>> validationErrors = null;
        try (JsonParser p = JSON_FACTORY.createParser(wire, 4, headerLen)) {
            if (p.nextToken() == JsonToken.START_OBJECT) {
                while (p.nextToken() == JsonToken.FIELD_NAME) {
                    String name = p.currentName();
                    JsonToken t = p.nextToken();
                    switch (name) {
                        case "status" -> status = p.getValueAsInt(500);
                        case "headers" -> {
                            if (t != JsonToken.START_OBJECT) { p.skipChildren(); break; }
                            while (p.nextToken() == JsonToken.FIELD_NAME) {
                                String k = p.currentName();
                                if (p.nextToken() == JsonToken.START_ARRAY) {
                                    List<String> list = new ArrayList<>();
                                    while (p.nextToken() != JsonToken.END_ARRAY) list.add(p.getValueAsString());
                                    headers.put(k, list);
                                } else {
                                    headers.put(k, p.getValueAsString());
                                }
                            }
                        }
                        case "metadata" -> {
                            if (t != JsonToken.START_OBJECT) { p.skipChildren(); break; }
                            while (p.nextToken() == JsonToken.FIELD_NAME) {
                                String k = p.currentName();
                                p.nextToken();
                                metadata.put(k, p.getValueAsString());
                            }
                        }
                        case "validation_errors" -> {
                            if (t != JsonToken.START_ARRAY) { p.skipChildren(); break; }
                            validationErrors = new ArrayList<>();
                            while (p.nextToken() == JsonToken.START_OBJECT) {
                                Map<String, Object> entry = new LinkedHashMap<>();
                                while (p.nextToken() == JsonToken.FIELD_NAME) {
                                    String k = p.currentName();
                                    p.nextToken();
                                    entry.put(k, p.getValueAsString());
                                }
                                validationErrors.add(entry);
                            }
                        }
                        default -> p.skipChildren();
                    }
                }
            }
        } catch (IOException e) {
            throw new IllegalArgumentException("wire header JSON parse failed", e);
        }
        int bodyStart = 4 + headerLen;
        ByteBuffer body = ByteBuffer.wrap(wire, bodyStart, wire.length - bodyStart)
                .slice().asReadOnlyBuffer();
        return new DecodedResponse(status, headers, metadata, body, validationErrors);
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
