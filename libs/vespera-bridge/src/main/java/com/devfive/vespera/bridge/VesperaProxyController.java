package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.channels.Channels;
import java.nio.channels.WritableByteChannel;
import java.nio.charset.StandardCharsets;
import java.util.Enumeration;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;

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
 * routing small bounded idempotent requests through the
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

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver) {
        this.appResolver = Objects.requireNonNull(appResolver, "appResolver");
        this.modeResolver = Objects.requireNonNull(modeResolver, "modeResolver");
    }

    @RequestMapping(value = "/**", consumes = MediaType.ALL_VALUE)
    public Object proxy(HttpServletRequest request,
                        HttpServletResponse response) throws IOException {

        final String appName = appResolver.resolveAppName(request);
        final DispatchMode mode = modeResolver.resolveMode(request);
        final String method = request.getMethod();
        final String path = request.getRequestURI();
        final String query = Objects.toString(request.getQueryString(), "");
        final Map<String, String> headers = collectHeaders(request);

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
                        readBody(request));
                return null;
            case ASYNC:
                return dispatchAsyncFlow(appName, method, path, query, headers,
                        readBody(request));
            case STREAMING:
                dispatchStreaming(response, appName, method, path, query,
                        headers, readBody(request));
                return null;
            case DIRECT:
                dispatchDirectMode(response, appName, method, path, query, headers,
                        readBody(request));
                return null;
            case BIDIRECTIONAL_STREAMING:
            default:
                dispatchBidirectional(request, response, appName, method, path, query, headers);
                return null;
        }
    }

    /** Shared empty body — avoids a {@code new byte[0]} per bodyless request. */
    private static final byte[] EMPTY_BODY = new byte[0];

    /**
     * Largest body for which {@link #readBody} trusts {@code
     * Content-Length} enough to pre-allocate the exact array.  Beyond
     * this (or for unknown length) it falls back to {@code readAllBytes},
     * which grows with the bytes actually present — so a lying / huge
     * {@code Content-Length} header cannot force a giant up-front
     * allocation.
     */
    private static final int MAX_FIXED_BODY = 64 * 1024 * 1024;

    // Package-private (not private) so unit tests can exercise the
    // bodyless fast path and length-based reads with MockHttpServletRequest.
    static byte[] readBody(HttpServletRequest request) throws IOException {
        // Provably bodyless requests skip the servlet InputStream
        // acquisition + readAllBytes allocations entirely. This covers
        // both Content-Length: 0 AND length-less GET/HEAD/OPTIONS (the
        // hottest path — the small idempotent GETs the SmartDispatch
        // resolver routes through DIRECT, which previously still paid a
        // getInputStream()+readAllBytes() round-trip on an empty body).
        if (DispatchModeResolver.definitelyBodyless(request)) {
            return EMPTY_BODY;
        }
        long contentLength = request.getContentLengthLong();
        try (InputStream in = request.getInputStream()) {
            if (contentLength > 0 && contentLength <= MAX_FIXED_BODY) {
                // Known, bounded length: one exact allocation filled in
                // place, skipping readAllBytes()'s grow-by-doubling and
                // its final trim copy.  readNBytes blocks until the
                // buffer is full or EOF; the servlet container caps the
                // stream at Content-Length, so a well-formed request
                // returns exactly contentLength bytes (a short read
                // yields a correctly-sized smaller array).
                return in.readNBytes((int) contentLength);
            }
            // Unknown (-1) or oversized length: faithful incremental read.
            return in.readAllBytes();
        }
    }

    /**
     * Synchronous dispatch — writes the wire response straight to the
     * servlet response (status + headers via {@link WireHeaderReader},
     * then the body region written directly from the wire array).  This
     * drops both the body-sized {@code Arrays.copyOfRange} and the
     * {@code ResponseEntity<byte[]>} object that the prior
     * {@link #buildResponseEntityFromWire} path allocated per response.
     * Mirrors {@link #dispatchDirectMode}; the async path still uses
     * {@code buildResponseEntityFromWire} (Spring async completion).
     */
    private static void dispatchSync(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) throws IOException {
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
        WireHeaderReader.apply(
                ByteBuffer.wrap(wire), 4, headerLen,
                response::setStatus, response::addHeader);
        int bodyOff = 4 + headerLen;
        int bodyLen = wire.length - bodyOff;
        if (bodyLen > 0) {
            if (!response.containsHeader("Content-Length")) {
                response.setContentLength(bodyLen);
            }
            response.getOutputStream().write(wire, bodyOff, bodyLen);
        }
        response.getOutputStream().flush();
    }

    private CompletableFuture<ResponseEntity<?>> dispatchAsyncFlow(
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        return VesperaBridge.dispatch(wireReq)
                .thenApply(VesperaProxyController::buildResponseEntityFromWire);
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
            Map<String, String> headers, byte[] body) throws IOException {
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
            Map<String, String> headers) throws IOException {
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
     * only for idempotent methods; for others a
     * {@link VesperaBridge.BufferTooSmallException} surfaces as a
     * {@code 500} with the required size — the controller never
     * double-executes a non-idempotent handler.  (The resolver should
     * keep such requests off DIRECT in the first place.)
     */
    private static void dispatchDirectMode(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) throws IOException {
        ByteBuffer wireResp;
        try {
            // Encodes straight into the pooled direct buffer — no
            // intermediate wire-sized byte[].
            wireResp = VesperaBridge.dispatchDirectPooled(
                    appName, method, path, query, headers, body, isIdempotent(method));
        } catch (VesperaBridge.BufferTooSmallException overflow) {
            // Non-idempotent + response larger than the pool: the first
            // dispatch already ran; its result was discarded.  Serving
            // via dispatchBytes would run the handler a second time, so
            // surface the size to the operator instead of silently
            // double-executing.  (The resolver should keep
            // non-idempotent methods off DIRECT in the first place.)
            response.setStatus(500);
            response.getOutputStream().write(
                    ("vespera DIRECT overflow: response needs "
                            + overflow.requiredSize()
                            + " bytes; route this request via BIDIRECTIONAL_STREAMING")
                            .getBytes(StandardCharsets.UTF_8));
            response.getOutputStream().flush();
            return;
        }

        // Commit status + headers parsed straight from the direct buffer —
        // no byte[] copy, no DecodedResponse object graph (maps / metadata /
        // body views). addHeader on the still-uncommitted response is
        // equivalent to setHeader for a header's first value and appends for
        // multi-valued headers (e.g. set-cookie).
        int headerLen = wireResp.getInt(0);
        WireHeaderReader.apply(wireResp, 4, headerLen, response::setStatus, response::addHeader);

        // Stream the body region of the direct buffer straight out.
        // Drain explicitly: WritableByteChannel.write() is contractually
        // permitted to perform a partial write, so loop until the buffer
        // is fully written rather than relying on the internal looping of
        // Channels.newChannel(OutputStream).  A single channel is created
        // and reused across the (normally one) iterations.  The channel
        // wraps a blocking servlet OutputStream, so each write makes
        // forward progress and the loop terminates.
        wireResp.position(4 + headerLen);
        if (wireResp.hasRemaining()) {
            WritableByteChannel bodyChannel =
                    Channels.newChannel(response.getOutputStream());
            while (wireResp.hasRemaining()) {
                bodyChannel.write(wireResp);
            }
        }
        response.getOutputStream().flush();
    }

    /** Idempotent per RFC 9110 — safe to re-run on DIRECT overflow retry. */
    private static boolean isIdempotent(String method) {
        return HttpMethods.isIdempotent(method);
    }

    // Package-private (not private) so unit tests can verify duplicate-header
    // joining (B4) with MockHttpServletRequest.
    static Map<String, String> collectHeaders(HttpServletRequest request) {
        // Pre-size for a typical request header count so the common case
        // never resizes; keep LinkedHashMap (NOT HashMap) so insertion
        // order — and thus the request header JSON field order — stays
        // deterministic.
        Map<String, String> headers = new LinkedHashMap<>(32);
        Enumeration<String> names = request.getHeaderNames();
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            headers.put(toLowerCaseAscii(name), joinHeaderValues(name, request));
        }
        return headers;
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
            return request.getHeader(name);
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
     * Lowercase an HTTP header name without allocating when it is
     * already lowercase — the common case, since HTTP/2 mandates
     * lowercase field names and most HTTP/1.1 clients send canonical
     * names.  Header names are ASCII per RFC 9110 §5.1, so an ASCII
     * scan is sufficient; only on encountering an uppercase letter do
     * we fall back to a full {@link String#toLowerCase} copy.
     */
    private static String toLowerCaseAscii(String name) {
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c >= 'A' && c <= 'Z') {
                return name.toLowerCase(Locale.ROOT);
            }
        }
        return name;
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
        int headerLen = buf.getInt(0);
        WireHeaderReader.apply(buf, 4, headerLen, response::setStatus, response::addHeader);
    }

    /**
     * Convert a fully-decoded sync/async wire response into a
     * Spring {@link ResponseEntity}.  Body is delivered as
     * {@link String} for text-like Content-Types,
     * {@code byte[]} otherwise.
     */
    /**
     * Build a {@link ResponseEntity} straight from the wire response
     * {@code byte[]} with minimal allocation:
     *
     * <ul>
     *   <li><b>status + headers</b> via the allocation-lean
     *       {@link WireHeaderReader} (parses directly to {@link HttpHeaders} —
     *       no {@code DecodedResponse} graph: no {@code metadata} map, no
     *       intermediate headers map, no body {@code ByteBuffer} views), and</li>
     *   <li><b>body</b> sliced once straight from the wire tail — for text this
     *       drops the intermediate {@code byte[]} that {@code bodyBytes()} would
     *       allocate (a body-sized copy avoided per text response, scaling with
     *       payload).</li>
     * </ul>
     *
     * <p>{@link VesperaBridge#decodeResponse(byte[])} stays the public API for
     * external/streaming consumers; this is a controller-internal fast path.
     * Pure Java (no JNI) — safe to run on the async completion thread.
     */
    private static ResponseEntity<?> buildResponseEntityFromWire(byte[] wire) {
        if (wire == null || wire.length < 4) {
            throw new IllegalArgumentException(
                    "wire response too short: " + (wire == null ? "null" : wire.length + " bytes"));
        }
        int headerLen = ((wire[0] & 0xFF) << 24) | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8) | (wire[3] & 0xFF);
        if (headerLen < 0 || (long) 4 + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen + " overflows response (" + wire.length + " bytes)");
        }
        HttpHeaders httpHeaders = new HttpHeaders();
        int[] statusHolder = {500};
        WireHeaderReader.apply(
                java.nio.ByteBuffer.wrap(wire),
                4,
                headerLen,
                s -> statusHolder[0] = s,
                httpHeaders::add);
        HttpStatus status = HttpStatus.valueOf(statusHolder[0]);
        // Deliver the body as byte[] for every content type.  The wire
        // header already carries the exact Content-Type, and Spring's
        // ByteArrayHttpMessageConverter writes it verbatim — so this
        // drops, for text responses, both the intermediate String
        // allocation AND the UTF-8 decode→re-encode round-trip that
        // ResponseEntity<String> performed (the StringHttpMessageConverter
        // would re-encode the just-decoded String straight back to UTF-8).
        // One body-sized slice copy remains: ResponseEntity<byte[]> needs
        // an owned array.  (BREAKING vs ≤0.2.0: text responses surface as
        // ResponseEntity<byte[]> rather than ResponseEntity<String>; the
        // bytes on the wire are identical.)
        int bodyOff = 4 + headerLen;
        return new ResponseEntity<>(
                java.util.Arrays.copyOfRange(wire, bodyOff, wire.length), httpHeaders, status);
    }
}
