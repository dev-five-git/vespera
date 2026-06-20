package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;
import org.springframework.core.io.AbstractResource;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.server.ResponseStatusException;

import java.io.IOException;
import java.io.ByteArrayInputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Enumeration;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import java.util.concurrent.ForkJoinPool;

/**
 * Catch-all proxy controller — autoconfigured by
 * {@link VesperaBridgeAutoConfiguration} when no user-supplied
 * {@code VesperaProxyController} bean is present (gated by
 * {@code vespera.bridge.controller-enabled}, default {@code true}).
 *
 * <p><strong>Endpoint contract:</strong> every URL published in
 * vespera's generated {@code openapi.json} is reachable through
 * Spring at the <strong>same URL</strong>.  No path prefix is
 * injected by this controller; routing transparently mirrors the
 * Rust router.
 *
 * <p>Per request, the controller delegates two decisions to the
 * configured strategies:
 *
 * <ol>
 *   <li>{@link AppNameResolver#resolveAppName(HttpServletRequest)}
 *       — which named Rust app should receive this request
 *       ({@code null} → default app).</li>
 *   <li>{@link DispatchModeResolver#resolveMode(HttpServletRequest)}
 *       — which {@link DispatchMode} JNI path to use.</li>
 * </ol>
 *
 * <p>The autoconfigured defaults ({@link HeaderAppNameResolver} on
 * {@code X-Vespera-App} + {@link SmartDispatchModeResolver} since
 * 0.2.0) keep the proxy transparent for every payload size while
 * routing small bounded safe requests through the
 * direct-buffer fast path (DIRECT 2.2 µs / SYNC 3.2 µs vs streaming
 * 24.1 µs on a small {@code GET /health}).  Restore the pre-0.2.0
 * bidirectional default with
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming}, or
 * replace either bean to change the policy without subclassing this
 * controller.
 */
@RestController
public class VesperaProxyController {

    private static final Logger log =
            LoggerFactory.getLogger(VesperaProxyController.class);

    private final AppNameResolver appResolver;
    private final DispatchModeResolver modeResolver;
    private final Executor asyncResponseExecutor;
    private final boolean directRetryOnOverflow;
    private final long maxBufferedRequestBytes;

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver) {
        this(appResolver, modeResolver, ForkJoinPool.commonPool(), true, 0);
    }

    public VesperaProxyController(AppNameResolver appResolver,
                                   DispatchModeResolver modeResolver,
                                   Executor asyncResponseExecutor,
                                   boolean directRetryOnOverflow) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow, 0);
    }

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                  Executor asyncResponseExecutor,
                                  boolean directRetryOnOverflow,
                                  long maxBufferedRequestBytes) {
        this.appResolver = Objects.requireNonNull(appResolver, "appResolver");
        this.modeResolver = Objects.requireNonNull(modeResolver, "modeResolver");
        this.asyncResponseExecutor = Objects.requireNonNull(asyncResponseExecutor, "asyncResponseExecutor");
        this.directRetryOnOverflow = directRetryOnOverflow;
        this.maxBufferedRequestBytes = Math.max(0, maxBufferedRequestBytes);
    }

    @RequestMapping(value = "/**", consumes = MediaType.ALL_VALUE)
    public Object proxy(HttpServletRequest request,
                        HttpServletResponse response) throws IOException {

        final RequestShape shape = RequestShape.capture(request);
        final String appName = VesperaWireCodec.normalizedAppName(appResolver.resolveAppName(request));
        final DispatchMode mode = modeResolver.resolveMode(request);
        final Boolean currentThreadIsVirtual = modeResolver instanceof SmartDispatchModeResolver
                ? SmartDispatchModeResolver.cachedCurrentThreadIsVirtual(request)
                : null;
        final String method = shape.method;
        // Path RELATIVE to the servlet context: a Spring app deployed under
        // a non-root context (e.g. server.servlet.context-path=/api) must
        // still forward `/health` — not `/api/health` — so the Rust router
        // sees exactly the URL published in the generated openapi.json.
        final String path = pathWithinApplication(request);
        final String query = Objects.toString(request.getQueryString(), "");
        final VesperaBridge.HeaderSource headers = sink -> forEachRequestHeader(request, sink);

        if (log.isDebugEnabled()) {
            log.debug("-> Rust  {} {} app={} mode={}", method, path, appName, mode);
        }

        // For bidirectional streaming, pass the servlet InputStream
        // straight through — DO NOT pre-read it.  For every other
        // mode, materialise the body bytes here (replaces Spring's
        // @RequestBody, which we cannot use because it would consume
        // the InputStream and leave the bidirectional path empty).
        switch (mode) {
            case SYNC:
                dispatchSync(response, appName, method, path, query, headers,
                        readBody(request, shape, maxBufferedRequestBytes));
                return null;
            case ASYNC:
                return dispatchAsyncFlow(appName, method, path, query, headers,
                        readBody(request, shape, maxBufferedRequestBytes));
            case STREAMING:
                // STREAMING materialises the REQUEST body (only the response
                // streams), so it must honour the same buffered-request cap
                // as SYNC/ASYNC/DIRECT — otherwise a custom resolver routing
                // a bodyful request here would bypass
                // vespera.bridge.max-buffered-request-bytes.
                dispatchStreaming(response, appName, method, path, query,
                        headers, readBody(request, shape, maxBufferedRequestBytes));
                return null;
            case DIRECT:
                dispatchDirectMode(response, appName, method, path, query, headers,
                        readBody(request, shape, maxBufferedRequestBytes), currentThreadIsVirtual);
                return null;
            case BIDIRECTIONAL_STREAMING:
            default:
                dispatchBidirectional(request, response, appName, method, path, query, headers);
                return null;
        }
    }

    /**
     * Resolve the request path RELATIVE to the servlet context path so a
     * Spring app deployed under a non-root context
     * ({@code server.servlet.context-path=/api}) still forwards the
     * context-relative URL the Rust router and the generated
     * {@code openapi.json} know — {@code /api/health} on the wire becomes
     * {@code /health}.  At the root context ({@code getContextPath()}
     * empty) the request URI is returned unchanged; a request to the bare
     * context root collapses to {@code "/"}.
     *
     * <p>Package-private so unit tests can verify it directly with
     * {@code MockHttpServletRequest}.
     */
    static String pathWithinApplication(HttpServletRequest request) {
        String uri = request.getRequestURI();
        String context = request.getContextPath();
        if (context == null || context.isEmpty() || !uri.startsWith(context)) {
            return uri;
        }
        // Only strip when the context is a whole leading path segment — the
        // servlet container guarantees this, but guard against a degenerate
        // `/apixyz` being mis-stripped against context `/api`.
        if (uri.length() > context.length() && uri.charAt(context.length()) != '/') {
            return uri;
        }
        String stripped = uri.substring(context.length());
        return stripped.isEmpty() ? "/" : stripped;
    }

    /**
     * Largest body for which {@link #readBody} trusts {@code
     * Content-Length} enough to pre-allocate the exact array.  Beyond
     * this (or for unknown length) it falls back to {@code readAllBytes},
     * which grows with the bytes actually present — so a lying / huge
     * {@code Content-Length} header cannot force a giant up-front
     * allocation.
     */
    private static final int MAX_FIXED_BODY = 64 * 1024 * 1024;

    /**
     * Largest body that can be materialised into a single Java {@code byte[]}
     * (the JVM array-length ceiling is just under {@link Integer#MAX_VALUE}).
     * A buffered request whose length provably exceeds this can never be read
     * via {@code readAllBytes}/{@code readNBytes}, so it is rejected with 413
     * rather than allowed to attempt an impossible allocation; such requests
     * must go through {@code BIDIRECTIONAL_STREAMING}.
     */
    private static final long MAX_BUFFERED_BODY = Integer.MAX_VALUE - 8L;

    private static final int DIRECT_BODY_SCRATCH_INITIAL = 16 * 1024;
    private static final int DIRECT_BODY_COPY_CHUNK = 1024 * 1024;
    private static final int DIRECT_BODY_SCRATCH_RETAIN_CAPACITY = 256 * 1024;
    private static final ThreadLocal<byte[]> DIRECT_BODY_SCRATCH =
            ThreadLocal.withInitial(() -> new byte[DIRECT_BODY_SCRATCH_INITIAL]);

    /**
     * Drop this thread's reusable heap scratch buffer used for DIRECT response
     * body copies. Intended for servlet-container shutdown/redeploy cleanup;
     * keep pooling active during request handling.
     */
    static void clearCurrentThreadBuffers() {
        DIRECT_BODY_SCRATCH.remove();
    }

    // Package-private (not private) so unit tests can exercise the
    // bodyless fast path and length-based reads with MockHttpServletRequest.
    static byte[] readBody(HttpServletRequest request) throws IOException {
        return readBody(request, 0);
    }

    static byte[] readBody(HttpServletRequest request, long maxBufferedRequestBytes)
            throws IOException {
        return readBody(request, RequestShape.from(request), maxBufferedRequestBytes);
    }

    static byte[] readBody(
            HttpServletRequest request, RequestShape shape, long maxBufferedRequestBytes)
            throws IOException {
        // Provably bodyless requests skip the servlet InputStream
        // acquisition + readAllBytes allocations entirely. This covers
        // both Content-Length: 0 AND length-less GET/HEAD/OPTIONS (the
        // hottest path — the small safe GETs the SmartDispatch
        // resolver routes through DIRECT, which previously still paid a
        // getInputStream()+readAllBytes() round-trip on an empty body).
        if (shape.definitelyBodyless) {
            return VesperaWireCodec.EMPTY_BODY;
        }
        long contentLength = shape.contentLength;
        long cap = Math.max(0, maxBufferedRequestBytes);
        if (cap > 0 && contentLength > cap) {
            throw payloadTooLarge(contentLength, cap);
        }
        // A buffered body must fit a single Java byte[] (≈ 2 GiB). A larger
        // known Content-Length can never be materialised here, so reject it
        // (413) instead of letting readAllBytes()/readNBytes() attempt an
        // impossible allocation and throw OutOfMemoryError. Such requests must
        // go through BIDIRECTIONAL_STREAMING.
        if (contentLength > MAX_BUFFERED_BODY) {
            throw payloadTooLarge(contentLength, MAX_BUFFERED_BODY);
        }
        try (InputStream in = request.getInputStream()) {
            if (cap > 0 && contentLength < 0) {
                long cappedPlusOne = cap == Long.MAX_VALUE ? Long.MAX_VALUE : cap + 1;
                long effectiveLimit = Math.min(cappedPlusOne, MAX_BUFFERED_BODY);
                int readLimit = (int) effectiveLimit;
                byte[] body = in.readNBytes(readLimit);
                if ((long) body.length > cap) {
                    throw payloadTooLarge(body.length, cap);
                }
                if ((long) body.length == MAX_BUFFERED_BODY && cap >= MAX_BUFFERED_BODY) {
                    throw payloadTooLarge(body.length, MAX_BUFFERED_BODY);
                }
                return body;
            }
            if (contentLength > 0 && (cap > 0 || contentLength <= MAX_FIXED_BODY)) {
                // Known, bounded length: one exact allocation filled in
                // place, skipping readAllBytes()'s grow-by-doubling and
                // its final trim copy.  readNBytes blocks until the
                // buffer is full or EOF; the servlet container caps the
                // stream at Content-Length, so a well-formed request
                // returns exactly contentLength bytes (a short read
                // yields a correctly-sized smaller array).
                return in.readNBytes((int) contentLength);
            }
            // Unknown (-1), or oversized known length with no explicit cap:
            // read incrementally, but still enforce the single-byte[] hard
            // ceiling so a custom resolver cannot grow the JVM heap until OOM.
            byte[] body = in.readNBytes((int) MAX_BUFFERED_BODY);
            if ((long) body.length == MAX_BUFFERED_BODY) {
                throw payloadTooLarge(body.length, MAX_BUFFERED_BODY);
            }
            return body;
        }
    }

    private static ResponseStatusException payloadTooLarge(long actualBytes, long capBytes) {
        return new ResponseStatusException(
                HttpStatus.PAYLOAD_TOO_LARGE,
                "buffered request body exceeds vespera.bridge.max-buffered-request-bytes="
                        + capBytes + " (actual " + actualBytes + " bytes)");
    }

    /**
     * Synchronous dispatch — writes the wire response straight to the
     * servlet response (status + headers via {@link WireHeaderReader},
     * then the body region written directly from the wire array).  This
     * drops both the body-sized {@code Arrays.copyOfRange} and the
     * {@code ResponseEntity<byte[]>} object that the prior
     * {@link #buildResponseEntityFromWire} path allocated per response.
     * Mirrors {@link #dispatchDirectMode}; the async path still uses
     * {@code buildResponseEntityFromWire} (Spring async completion), but
     * returns a zero-copy {@code Resource} view over the wire body.
     */
    private static void dispatchSync(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body) throws IOException {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        byte[] wireResp = VesperaBridge.dispatchBytes(wireReq);
        writeWireResponse(wireResp, response);
    }

    /**
     * Write a complete wire response ({@code [u32 BE header_len | JSON
     * header | body]}) straight to the servlet response: status + headers
     * applied from the header region via the allocation-lean
     * {@link WireHeaderReader}, then the body region written directly from
     * {@code wire} with no {@code byte[]} slice copy.  The exact body
     * length is known, so {@code Content-Length} is set when the wire
     * header did not already carry it — preserving the prior
     * {@code ResponseEntity<byte[]>} behaviour without the copy.
     */
    private static void writeWireResponse(byte[] wire, HttpServletResponse response)
            throws IOException {
        int headerLen = VesperaWireCodec.readHeaderLength(wire);
        int[] statusHolder = {500};
        WireHeaderReader.apply(
                ByteBuffer.wrap(wire), 4, headerLen,
                s -> {
                    statusHolder[0] = s;
                    response.setStatus(s);
                },
                (n, v) -> addServletResponseHeader(response, n, v));
        int bodyOff = 4 + headerLen;
        int bodyLen = wire.length - bodyOff;
        if (bodyLen > 0) {
            if (!response.containsHeader("Content-Length")) {
                response.setContentLength(bodyLen);
            }
            response.getOutputStream().write(wire, bodyOff, bodyLen);
        } else if (responseStatusPermitsBody(statusHolder[0])
                && !response.containsHeader("Content-Length")) {
            response.setContentLength(0);
        }
    }

    private CompletableFuture<ResponseEntity<?>> dispatchAsyncFlow(
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body) {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        return VesperaBridge.dispatch(wireReq)
                .thenApplyAsync(
                        VesperaProxyController::buildResponseEntityFromWire,
                        asyncResponseExecutor);
    }

    /**
     * Response-only streaming — request body materialised, response
     * streams chunk-by-chunk to the servlet output stream.  Status
     * and headers commit through the JNI header callback BEFORE the
     * first body byte hits the wire.
     */
    private void dispatchStreaming(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body) throws IOException {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        VesperaBridge.dispatchStreamingWithHeader(
                wireReq,
                headerBytes -> applyDecodedHeader(headerBytes, response),
                response.getOutputStream());
        response.getOutputStream().flush();
    }

    /**
     * Bidirectional streaming — both request body (from
     * {@code request.getInputStream()}) and response body (to
     * {@code response.getOutputStream()}) flow chunk-by-chunk.
     * 1 GiB ↔ 1 GiB transfers run in {@code O(chunk_size)} RAM on
     * both Rust and JVM sides.
     */
    private void dispatchBidirectional(
            HttpServletRequest request, HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers) throws IOException {
        byte[] wireHeader = VesperaBridge.encodeRequestHeader(
                appName, method, path, query, headers);
        VesperaBridge.dispatchFullStreamingWithHeader(
                wireHeader,
                headerBytes -> applyDecodedHeader(headerBytes, response),
                request.getInputStream(),
                response.getOutputStream());
        response.getOutputStream().flush();
    }

    /**
     * Direct-buffer dispatch — request body materialised (DIRECT is
     * gated to small bounded payloads by the resolver), response served
     * from the pooled direct buffer without a {@code byte[]}
     * materialisation: the header slice is decoded to commit
     * status/headers, then the body region is channelled straight into
     * the servlet output stream.
     *
     * <p>Overflow retry (which re-runs the Rust handler) is permitted
     * only for <em>safe</em> methods (GET/HEAD/OPTIONS), which are not
     * intended to mutate server state. The replayed response may still
     * differ (for example timestamps or generated request IDs); for every
     * other method — including
     * idempotent-but-unsafe PUT/DELETE, whose second run can return a
     * different response (e.g. DELETE → 204 then 404) — a
     * {@link VesperaBridge.BufferTooSmallException} surfaces as a
     * {@code 500} with the required size, so the controller never
     * double-executes a handler whose response could change.
     */
    private void dispatchDirectMode(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body,
            Boolean currentThreadIsVirtual) throws IOException {
        if (!isSafe(method)) {
            // DIRECT runs the Rust handler on the FIRST dispatch before any
            // overflow is known; for an UNSAFE method an overflow would 500
            // *after* the side effect already happened (partial unsafe
            // execution).  A custom DispatchModeResolver can route an unsafe
            // method here, so gate it at the controller boundary: serve unsafe
            // requests via SYNC, which never re-runs the handler.
            dispatchSync(response, appName, method, path, query, headers, body);
            return;
        }
        ByteBuffer wireResp;
        try {
            // Encodes straight into the pooled direct buffer — no
            // intermediate wire-sized byte[].
            boolean retry = directRetryOnOverflow && isSafe(method);
            wireResp = currentThreadIsVirtual == null
                    ? VesperaBridge.dispatchDirectPooled(
                            appName, method, path, query, headers, body, retry)
                    : VesperaBridge.dispatchDirectPooled(
                            appName, method, path, query, headers, body,
                            retry, currentThreadIsVirtual.booleanValue());
        } catch (VesperaBridge.BufferTooSmallException overflow) {
            // The first dispatch already ran; its oversized result was discarded.
            if (isSafe(method) && directRetryOnOverflow) {
                // Safe method + retry enabled: the response is larger than the
                // pooled direct buffer's hard cap. Re-route through response
                // streaming so a large download streams chunk-by-chunk instead
                // of being heap-buffered — the prior dispatchBytes fallback
                // could spike the JVM heap (OOM) on multi-GiB responses. A safe
                // re-run is not intended to mutate state, but its response may
                // differ (timestamps, random IDs). The DIRECT path has not
                // committed yet, so streaming takes over cleanly.
                dispatchStreaming(response, appName, method, path, query, headers, body);
                return;
            }
            // Unsafe method (or retry disabled): re-running could return a
            // different response (e.g. DELETE → 204 then 404), so surface the
            // size to the operator instead of silently double-executing.
            byte[] error = ("vespera DIRECT overflow: response needs "
                    + overflow.requiredSize()
                    + " bytes; route this request via BIDIRECTIONAL_STREAMING")
                    .getBytes(StandardCharsets.UTF_8);
            response.setStatus(500);
            response.setContentType("text/plain; charset=utf-8");
            response.setContentLength(error.length);
            response.getOutputStream().write(error);
            response.getOutputStream().flush();
            return;
        }

        // Commit status + headers parsed straight from the direct buffer —
        // no byte[] copy, no DecodedResponse object graph (maps / metadata /
        // body views). addHeader on the still-uncommitted response is
        // equivalent to setHeader for a header's first value and appends for
        // multi-valued headers (e.g. set-cookie).
        int bodyLen = applyDirectHeaderAndPositionBody(wireResp, response);

        // Stream the body region of the direct buffer with an explicit
        // per-thread heap scratch.  Channels.newChannel(OutputStream)
        // allocates its own temporary heap buffer for direct-buffer writes;
        // keeping the scratch here makes the copy strategy predictable and
        // avoids one allocation per DIRECT response.  Loop until the whole
        // ByteBuffer region is consumed before flushing/committing.
        if (bodyLen > 0) {
            writeDirectBody(wireResp, response.getOutputStream());
        }
    }

    /**
     * Read and validate the wire header length prefix against the actual
     * buffer length BEFORE {@link WireHeaderReader#apply} indexes into it.
     * The direct / streaming callback paths receive these bytes straight
     * from native Rust; a malformed length (negative, or overrunning the
     * buffer) must surface as a clear {@link IllegalArgumentException}
     * rather than an {@link IndexOutOfBoundsException} escaping mid-response.
     * Mirrors the guard the heap {@code byte[]} paths
     * ({@link #writeWireResponse}, {@link #buildResponseEntityFromWire})
     * already apply.
     */
    static int readValidatedHeaderLen(ByteBuffer wire) {
        // Delegates to the single source of truth in VesperaWireCodec so the
        // u32 BE prefix decode + bounds contract stays byte-identical across
        // every wire-frame split site (heap byte[] and direct ByteBuffer). The
        // helper decodes from absolute bytes (order-independent) — never
        // wire.getInt(0), which honours the buffer's CURRENT byte order — so a
        // LITTLE_ENDIAN view can never misparse the big-endian wire prefix.
        return VesperaWireCodec.readHeaderLength(wire);
    }

    // Package-private so tests can verify DIRECT header/body-length behavior
    // without invoking the native dispatchDirect JNI symbol.
    static int applyDirectHeaderAndPositionBody(
            ByteBuffer wireResp, HttpServletResponse response) {
        int headerLen = readValidatedHeaderLen(wireResp);
        int[] statusHolder = {500};
        WireHeaderReader.apply(
                wireResp,
                4,
                headerLen,
                s -> {
                    statusHolder[0] = s;
                    response.setStatus(s);
                },
                (n, v) -> addServletResponseHeader(response, n, v));
        int bodyOff = 4 + headerLen;
        int bodyLen = wireResp.limit() - bodyOff;
        if (bodyLen > 0 && !response.containsHeader("Content-Length")) {
            response.setContentLength(bodyLen);
        } else if (bodyLen == 0
                && responseStatusPermitsBody(statusHolder[0])
                && !response.containsHeader("Content-Length")) {
            response.setContentLength(0);
        }
        wireResp.position(bodyOff);
        return bodyLen;
    }

    private static boolean responseStatusPermitsBody(int status) {
        return (status < 100 || status >= 200) && status != 204 && status != 304;
    }

    /**
     * Pure hop-by-hop response headers the proxy must NOT forward verbatim from
     * the Rust wire response. Forwarding a handler-supplied (or malicious
     * native) {@code transfer-encoding} / {@code connection} desynchronises
     * framing at the servlet container or a downstream proxy (e.g. a wire
     * {@code transfer-encoding: chunked} on a response the container frames with
     * {@code Content-Length}). These are connection-scoped per RFC 9110 and are
     * never legitimately emitted by an application handler.
     *
     * <p>{@code content-length} is deliberately NOT in this set: the Rust
     * handler is authoritative for it and the direct/buffered paths preserve a
     * wire-supplied length (locked by
     * {@code ProxyControllerBodyHeaderTest.directHeaderPreservesWireContentLength}),
     * synthesising it from the body only when absent.
     *
     * <p>Names are compared case-insensitively against the canonical lowercase
     * form the wire header carries.
     */
    private static final java.util.Set<String> HOP_BY_HOP_RESPONSE_HEADERS = java.util.Set.of(
            "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
            "te", "trailer", "transfer-encoding", "upgrade");

    static boolean isHopByHopResponseHeader(String name) {
        return HOP_BY_HOP_RESPONSE_HEADERS.contains(name.toLowerCase(java.util.Locale.ROOT));
    }

    /**
     * Apply a Rust wire response header to the servlet response, dropping the
     * hop-by-hop / framing headers the proxy owns ({@link #HOP_BY_HOP_RESPONSE_HEADERS}).
     */
    private static void addServletResponseHeader(
            HttpServletResponse response, String name, String value) {
        if (!isHopByHopResponseHeader(name)) {
            response.addHeader(name, value);
        }
    }

    private static void writeDirectBody(ByteBuffer body, OutputStream out) throws IOException {
        try {
            byte[] scratch = directBodyScratch(Math.min(body.remaining(), DIRECT_BODY_COPY_CHUNK));
            while (body.hasRemaining()) {
                int n = Math.min(body.remaining(), scratch.length);
                body.get(scratch, 0, n);
                out.write(scratch, 0, n);
            }
        } finally {
            shrinkDirectBodyScratchIfOversized();
        }
    }

    private static byte[] directBodyScratch(int required) {
        byte[] scratch = DIRECT_BODY_SCRATCH.get();
        if (scratch.length > DIRECT_BODY_SCRATCH_RETAIN_CAPACITY) {
            scratch = new byte[DIRECT_BODY_SCRATCH_INITIAL];
            DIRECT_BODY_SCRATCH.set(scratch);
        }
        if (scratch.length < required) {
            scratch = new byte[Math.min(DIRECT_BODY_COPY_CHUNK, required)];
            DIRECT_BODY_SCRATCH.set(scratch);
        }
        return scratch;
    }

    private static void shrinkDirectBodyScratchIfOversized() {
        if (DIRECT_BODY_SCRATCH.get().length > DIRECT_BODY_SCRATCH_RETAIN_CAPACITY) {
            DIRECT_BODY_SCRATCH.set(new byte[DIRECT_BODY_SCRATCH_INITIAL]);
        }
    }

    /**
     * "Safe" per RFC 9110 (GET/HEAD/OPTIONS) — not intended to mutate server
     * state, so the DIRECT overflow retry is allowed even though the replayed
     * response may differ (timestamps, random IDs). Idempotent-but-unsafe
     * methods (PUT/DELETE) are intentionally excluded: their second run can
     * return a different response (e.g. DELETE → 204 then 404), so on overflow
     * they fail with {@link VesperaBridge.BufferTooSmallException} instead of
     * auto-retrying and silently double-executing.
     */
    private static boolean isSafe(String method) {
        return HttpMethods.isSafe(method);
    }

    // Package-private (not private) so unit tests can verify duplicate-header
    // joining (B4) with MockHttpServletRequest.
    static Map<String, String> collectHeaders(HttpServletRequest request) {
        // Pre-size for a typical request header count so the common case
        // never resizes; keep LinkedHashMap (NOT HashMap) so insertion
        // order — and thus the request header JSON field order — stays
        // deterministic.
        Map<String, String> headers = new LinkedHashMap<>(32);
        forEachRequestHeader(request, headers::put);
        return headers;
    }

    static void forEachRequestHeader(HttpServletRequest request, VesperaBridge.HeaderSink sink) {
        Enumeration<String> names = request.getHeaderNames();
        // The Servlet spec permits getHeaderNames() to return null when the
        // container disallows header access; treat that as "no headers"
        // rather than letting a NullPointerException turn a recoverable case
        // into an HTTP 500.
        if (names == null) {
            return;
        }
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            sink.put(canonicalLowerHeaderName(name), joinHeaderValues(name, request));
        }
    }

    /**
     * Combine every value of a repeated request header so duplicates are
     * not silently dropped before Rust sees them (the prior
     * {@code request.getHeader(name)} returned only the first value).
     *
     * <p>The single-value case — the overwhelming majority of headers —
     * returns the lone value with no allocation.  Multiple same-name
     * values are combined per RFC 7230 §3.2.2 with {@code ", "}, except
     * {@code Cookie}, whose values themselves contain commas and must be
     * joined with {@code "; "} per RFC 6265bis §5.4 so the Rust cookie
     * parser still receives a valid cookie string.
     */
    private static String joinHeaderValues(String name, HttpServletRequest request) {
        Enumeration<String> values = request.getHeaders(name);
        if (values == null || !values.hasMoreElements()) {
            // A non-conformant container can return an empty getHeaders(name)
            // AND a null getHeader(name) for a name that getHeaderNames()
            // listed; coalesce to "" so a null never reaches the wire-header
            // JSON encoder (VesperaWireCodec.writeJsonString) and NPEs there.
            String value = request.getHeader(name);
            return value != null ? value : "";
        }
        String first = values.nextElement();
        if (!values.hasMoreElements()) {
            return first;
        }
        String separator = name.equalsIgnoreCase("cookie") ? "; " : ", ";
        StringBuilder sb = new StringBuilder(first);
        do {
            sb.append(separator).append(values.nextElement());
        } while (values.hasMoreElements());
        return sb.toString();
    }

    /**
     * Lowercase an HTTP header name while avoiding per-request lowercase
     * allocations for common HTTP/1.1 canonical names. Header names are ASCII
     * per RFC 9110 §5.1, so uncommon names fall back to a small ASCII copy only
     * when they contain uppercase bytes.
     */
    private static String canonicalLowerHeaderName(String name) {
        switch (name) {
            case "Host": return "host";
            case "Content-Type": return "content-type";
            case "Content-Length": return "content-length";
            case "Accept": return "accept";
            case "Accept-Encoding": return "accept-encoding";
            case "Accept-Language": return "accept-language";
            case "Authorization": return "authorization";
            case "Connection": return "connection";
            case "Cookie": return "cookie";
            case "User-Agent": return "user-agent";
            case "Referer": return "referer";
            case "Origin": return "origin";
            case "Cache-Control": return "cache-control";
            case "If-None-Match": return "if-none-match";
            case "If-Modified-Since": return "if-modified-since";
            case "X-Forwarded-For": return "x-forwarded-for";
            case "X-Forwarded-Host": return "x-forwarded-host";
            case "X-Forwarded-Proto": return "x-forwarded-proto";
            case "X-Request-Id": return "x-request-id";
            default: break;
        }
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c >= 'A' && c <= 'Z') {
                return toLowerCaseAscii(name);
            }
        }
        return name;
    }

    private static String toLowerCaseAscii(String name) {
        char[] chars = name.toCharArray();
        for (int i = 0; i < chars.length; i++) {
            char c = chars[i];
            if (c >= 'A' && c <= 'Z') {
                chars[i] = (char) (c + ('a' - 'A'));
            }
        }
        return new String(chars);
    }

    /**
     * Apply a decoded wire header to {@link HttpServletResponse} —
     * called from streaming dispatch callbacks BEFORE the first body
     * byte is written, while the response is still uncommitted.
     */
    private static void applyDecodedHeader(byte[] headerBytes,
                                            HttpServletResponse response) {
        // Apply status + headers straight from the wire header bytes via
        // the allocation-lean WireHeaderReader — the same path
        // dispatchDirectMode uses.  This avoids the DecodedResponse object
        // graph (headers map, the always-allocated metadata LinkedHashMap,
        // and the body ByteBuffer view) that VesperaBridge.decodeResponse
        // builds, on every streaming dispatch's header callback.
        // addHeader on an uncommitted response equals setHeader for a
        // header's first value and appends for multi-valued headers
        // (e.g. set-cookie), preserving the prior semantics.
        ByteBuffer buf = ByteBuffer.wrap(headerBytes);
        int headerLen = readValidatedHeaderLen(buf);
        WireHeaderReader.apply(
                buf, 4, headerLen,
                response::setStatus,
                (n, v) -> addServletResponseHeader(response, n, v));
    }

    /**
     * Build a {@link ResponseEntity} straight from the wire response
     * {@code byte[]} with minimal allocation:
     *
     * <ul>
     *   <li><b>status + headers</b> via the allocation-lean
     *       {@link WireHeaderReader} (parses directly to {@link HttpHeaders} —
     *       no {@code DecodedResponse} graph: no {@code metadata} map, no
     *       intermediate headers map, no body {@code ByteBuffer} views), and</li>
     *   <li><b>body</b> exposed as a {@link org.springframework.core.io.Resource}
     *       view over the wire tail — no body-sized {@code byte[]} slice copy.</li>
     * </ul>
     *
     * <p>{@link VesperaBridge#decodeResponse(byte[])} stays the public API for
     * external/streaming consumers; this is a controller-internal fast path.
     * Pure Java (no JNI) — run by the controller on its configured async
     * response executor instead of the native completion thread.
     */
    private static ResponseEntity<?> buildResponseEntityFromWire(byte[] wire) {
        int headerLen = VesperaWireCodec.readHeaderLength(wire);
        HttpHeaders httpHeaders = new HttpHeaders();
        int[] statusHolder = {500};
        WireHeaderReader.apply(
                java.nio.ByteBuffer.wrap(wire),
                4,
                headerLen,
                s -> statusHolder[0] = s,
                (n, v) -> {
                    if (!isHopByHopResponseHeader(n)) {
                        httpHeaders.add(n, v);
                    }
                });
        HttpStatusCode status = HttpStatusCode.valueOf(statusHolder[0]);
        int bodyOff = 4 + headerLen;
        return new ResponseEntity<>(
                new WireBodyResource(wire, bodyOff, wire.length - bodyOff), httpHeaders, status);
    }

    static final class WireBodyResource extends AbstractResource {
        private final byte[] wire;
        private final int offset;
        private final int length;

        WireBodyResource(byte[] wire, int offset, int length) {
            this.wire = Objects.requireNonNull(wire, "wire");
            this.offset = offset;
            this.length = length;
        }

        @Override
        public InputStream getInputStream() {
            return new ByteArrayInputStream(wire, offset, length);
        }

        @Override
        public long contentLength() {
            return length;
        }

        @Override
        public String getDescription() {
            return "vespera wire response body slice";
        }
    }
}
