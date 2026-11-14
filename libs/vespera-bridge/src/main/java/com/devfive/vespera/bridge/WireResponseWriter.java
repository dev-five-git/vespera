package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletResponse;
import org.springframework.core.io.InputStreamResource;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.ResponseEntity;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.Objects;

/**
 * Wire response → servlet / Spring response writer shared by every
 * {@link VesperaProxyController} dispatch mode (sync, direct, response
 * streaming, bidirectional streaming, async).
 *
 * <p>Owns three response-commit concerns so they stay identical across modes:
 *
 * <ul>
 *   <li>applying the wire header (status + filtered headers) through
 *       {@link HeaderPolicy.ResponseHeaderAccumulator};</li>
 *   <li>the body-permission gate (no body for {@code 1xx}/{@code 204}/{@code 304}
 *       statuses or {@code HEAD} requests);</li>
 *   <li>the proxy-owned {@code Content-Length}: the exact body length written
 *       for buffered GET-like responses, and — for {@code HEAD} — the
 *       representation length the Rust side declared. axum answers {@code HEAD}
 *       with an <em>empty</em> body but the {@code content-length} of the
 *       GET-equivalent response, so the empty wire body must not be advertised
 *       as {@code Content-Length: 0}.</li>
 * </ul>
 *
 * <p>Package-private: an internal collaborator of the proxy controller, not
 * public API.
 */
final class WireResponseWriter {
    private WireResponseWriter() {}

    /** Sentinel for "do not set a Content-Length" (length unknown). */
    static final long NO_CONTENT_LENGTH = -1L;

    // ── Body-permission + Content-Length policy ───────────────────────────

    static boolean responseStatusPermitsBody(int status) {
        return (status < 100 || status >= 200) && status != 204 && status != 304;
    }

    static boolean requestMethodPermitsBody(String method) {
        return method == null || !isHead(method);
    }

    static boolean responsePermitsBody(int status, String method) {
        return responseStatusPermitsBody(status) && requestMethodPermitsBody(method);
    }

    private static boolean isHead(String method) {
        return method != null && "HEAD".equalsIgnoreCase(method);
    }

    /**
     * The {@code Content-Length} a buffered (sync / direct / async) response
     * advertises, or {@link #NO_CONTENT_LENGTH} to leave it unset.
     *
     * <ul>
     *   <li>No-body status ({@code 1xx}/{@code 204}/{@code 304}) → unset: a
     *       {@code 1xx}/{@code 204} must not carry {@code Content-Length},
     *       and a {@code 304}'s would describe the selected representation,
     *       not the (empty) message body, so a synthesised {@code 0} is
     *       wrong for all of them (RFC 9110 §8.6).</li>
     *   <li>{@code HEAD} → the Rust-declared {@code content-length} (the
     *       GET-equivalent representation length axum computes) when present
     *       and valid; otherwise the wire body length if the handler produced
     *       one; otherwise unset (unknown length — never a misleading
     *       {@code 0}).</li>
     *   <li>Everything else → the exact body bytes written.</li>
     * </ul>
     */
    static long bufferedContentLength(
            int status, String method, long bodyLen, long declaredContentLength) {
        if (!responseStatusPermitsBody(status)) {
            return NO_CONTENT_LENGTH;
        }
        if (isHead(method)) {
            if (declaredContentLength >= 0) {
                return declaredContentLength;
            }
            return bodyLen > 0 ? bodyLen : NO_CONTENT_LENGTH;
        }
        return bodyLen;
    }

    // ── Header application ────────────────────────────────────────────────

    /**
     * Unified wire-header → servlet-response sink shared by every dispatch
     * mode's response-commit site (sync, direct, response-streaming header
     * callback). Builds a {@link HeaderPolicy.ResponseHeaderAccumulator} in
     * one pass — its {@code accept} captures every Connection-nominated
     * token (and the declared {@code content-length}) as a side-effect —
     * then applies the buffered headers through
     * {@link HeaderPolicy#addServletResponseHeaders}, which honours those
     * tokens and drops the proxy-owned {@code content-length}. The status is
     * set AFTER the whole header is parsed (apply() returns it, default 500).
     */
    private static HeaderPolicy.ResponseHeaderAccumulator applyWireHeaderToResponse(
            byte[] wire, int off, int len, HttpServletResponse response) {
        HeaderPolicy.ResponseHeaderAccumulator acc = new HeaderPolicy.ResponseHeaderAccumulator();
        acc.status = WireHeaderReader.apply(wire, off, len, acc);
        return commitHeader(acc, response);
    }

    private static HeaderPolicy.ResponseHeaderAccumulator applyWireHeaderToResponse(
            ByteBuffer wire, int off, int len, HttpServletResponse response) {
        HeaderPolicy.ResponseHeaderAccumulator acc = new HeaderPolicy.ResponseHeaderAccumulator();
        acc.status = WireHeaderReader.apply(wire, off, len, acc);
        return commitHeader(acc, response);
    }

    private static HeaderPolicy.ResponseHeaderAccumulator commitHeader(
            HeaderPolicy.ResponseHeaderAccumulator acc, HttpServletResponse response) {
        response.setStatus(acc.status);
        HeaderPolicy.addServletResponseHeaders(response, acc);
        if (acc.panicked) {
            ProxyServletResponse.markPanicked(response);
        }
        return acc;
    }

    /**
     * Sibling of {@link #applyWireHeaderToResponse(byte[], int, int, HttpServletResponse)}
     * that emits into a Spring {@link HttpHeaders} — used by
     * {@link #buildResponseEntityFromWire} on the ASYNC dispatch path, with
     * the same hop-by-hop / content-length / Connection-nominated filtering.
     */
    private static HeaderPolicy.ResponseHeaderAccumulator applyWireHeaderToHttpHeaders(
            byte[] wire, int off, int len, HttpHeaders httpHeaders) {
        HeaderPolicy.ResponseHeaderAccumulator acc = new HeaderPolicy.ResponseHeaderAccumulator();
        acc.status = WireHeaderReader.apply(wire, off, len, acc);
        for (HeaderPolicy.HeaderPair header : acc.headers) {
            if (!HeaderPolicy.isHopByHopResponseHeader(header.name())
                    && !HeaderPolicy.isContentLengthHeader(header.name())
                    && !HeaderPolicy.isConnectionNominatedHeader(header.name(), acc.connectionTokens)) {
                httpHeaders.add(header.name(), header.value());
            }
        }
        return acc;
    }

    private static void setServletContentLength(HttpServletResponse response, long length) {
        if (length >= 0) {
            response.setContentLengthLong(length);
        }
    }

    // ── Buffered (sync) ───────────────────────────────────────────────────

    /**
     * Write a complete wire response ({@code [u32 BE header_len | JSON
     * header | body]}) straight to the servlet response: status + headers
     * applied from the header region via the allocation-lean
     * {@link WireHeaderReader}, then the body region written directly from
     * {@code wire} with no {@code byte[]} slice copy.
     */
    static void writeWireResponse(byte[] wire, HttpServletResponse response)
            throws IOException {
        writeWireResponse(wire, response, null);
    }

    static void writeWireResponse(byte[] wire, HttpServletResponse response, String method)
            throws IOException {
        int headerLen = VesperaWireCodec.readHeaderLength(wire);
        HeaderPolicy.ResponseHeaderAccumulator acc =
                applyWireHeaderToResponse(wire, 4, headerLen, response);
        int bodyOff = 4 + headerLen;
        int bodyLen = wire.length - bodyOff;
        setServletContentLength(response, bufferedContentLength(
                acc.status, method, bodyLen, acc.declaredContentLength()));
        if (responsePermitsBody(acc.status, method) && bodyLen > 0) {
            response.getOutputStream().write(wire, bodyOff, bodyLen);
        }
    }

    // ── Direct buffer ─────────────────────────────────────────────────────

    /**
     * Read and validate the wire header length prefix against the actual
     * buffer length BEFORE {@link WireHeaderReader#apply} indexes into it, so
     * a malformed native length surfaces as {@link IllegalArgumentException}
     * rather than an {@link IndexOutOfBoundsException} mid-response.
     */
    static int readValidatedHeaderLen(ByteBuffer wire) {
        return VesperaWireCodec.readHeaderLength(wire);
    }

    static int applyDirectHeaderAndPositionBody(
            ByteBuffer wireResp, HttpServletResponse response) {
        return applyDirectHeaderAndPositionBody(wireResp, response, null);
    }

    /**
     * Commit status + headers from the direct buffer, set the proxy-owned
     * {@code Content-Length}, position the buffer at the body region, and
     * return how many body bytes must be written (0 when the status or
     * method forbids a body).
     */
    static int applyDirectHeaderAndPositionBody(
            ByteBuffer wireResp, HttpServletResponse response, String method) {
        int headerLen = readValidatedHeaderLen(wireResp);
        HeaderPolicy.ResponseHeaderAccumulator acc =
                applyWireHeaderToResponse(wireResp, 4, headerLen, response);
        int bodyOff = 4 + headerLen;
        int bodyLen = wireResp.limit() - bodyOff;
        setServletContentLength(response, bufferedContentLength(
                acc.status, method, bodyLen, acc.declaredContentLength()));
        wireResp.position(bodyOff);
        return responsePermitsBody(acc.status, method) ? bodyLen : 0;
    }

    // ── Streaming header callback ─────────────────────────────────────────

    /**
     * Apply a decoded wire header to {@link HttpServletResponse} — called
     * from streaming dispatch callbacks BEFORE the first body byte is
     * written, while the response is still uncommitted. Streaming GET-like
     * responses let the container frame the body (no {@code Content-Length});
     * a {@code HEAD} with a body-permitting status advertises the
     * Rust-declared representation length instead, since no body follows.
     * Returns whether body bytes may be written.
     */
    static boolean applyDecodedHeader(byte[] headerBytes,
                                      HttpServletResponse response,
                                      String method) {
        int headerLen = VesperaWireCodec.readHeaderLength(headerBytes);
        HeaderPolicy.ResponseHeaderAccumulator acc =
                applyWireHeaderToResponse(headerBytes, 4, headerLen, response);
        if (isHead(method) && responseStatusPermitsBody(acc.status)) {
            setServletContentLength(response, acc.declaredContentLength());
        }
        return responsePermitsBody(acc.status, method);
    }

    // ── Async (ResponseEntity) ────────────────────────────────────────────

    /**
     * Build a {@link ResponseEntity} straight from the wire response: status
     * + headers via {@link WireHeaderReader} (no {@code DecodedResponse}
     * graph) and the body as a {@code byte[]} copy of the wire tail. A plain
     * {@code byte[]} body (not a {@code Resource}) keeps Spring from adding
     * {@code Accept-Ranges}, answering {@code Range} requests with {@code 206}
     * or guessing a {@code Content-Type} from a resource name, so ASYNC
     * responses match SYNC / DIRECT. Pure Java — run on the controller's
     * async response executor, never the native completion thread.
     */
    static ResponseEntity<byte[]> buildResponseEntityFromWire(byte[] wire) {
        return buildResponseEntityFromWire(wire, null);
    }

    static ResponseEntity<byte[]> buildResponseEntityFromWire(byte[] wire, String method) {
        return buildResponseEntityFromWire(wire, method, null);
    }

    /**
     * As {@link #buildResponseEntityFromWire(byte[], String)}, also flagging a
     * caught Rust panic on {@code proxied} and echoing its request id unless
     * the Rust response set that header itself ({@code proxied} may be
     * {@code null}).
     */
    static ResponseEntity<byte[]> buildResponseEntityFromWire(
            byte[] wire, String method, ProxyServletResponse proxied) {
        EntityHead head = entityHead(wire, method, proxied);
        // Builder, not the (body, HttpHeaders, status) constructor: that
        // overload is new in Spring Framework 7, so linking against it would
        // throw NoSuchMethodError on a Spring Boot 3.x (Framework 6) runtime.
        return ResponseEntity.status(head.status)
                .headers(head.headers)
                .body(Arrays.copyOfRange(wire, head.bodyOff, head.bodyOff + head.bodyLen));
    }

    /**
     * The ASYNC response entity without copying the body out of the wire
     * array: a non-empty body is an {@link InputStreamResource} over a
     * {@link ByteArrayInputStream} window of {@code wire}, which Spring's
     * {@code ResourceHttpMessageConverter} writes with one
     * {@code transferTo} call.  Exactly {@code InputStreamResource} (not a
     * subclass): Spring skips {@code Range} / {@code Accept-Ranges} handling
     * for that type, so the response matches SYNC / DIRECT like the
     * {@code byte[]} variant ({@link #buildResponseEntityFromWire}).
     */
    static ResponseEntity<?> buildAsyncResponseEntityFromWire(
            byte[] wire, String method, ProxyServletResponse proxied) {
        EntityHead head = entityHead(wire, method, proxied);
        ResponseEntity.BodyBuilder builder = ResponseEntity.status(head.status).headers(head.headers);
        if (!responseStatusPermitsBody(head.status.value())) {
            // No body object at all: a message converter would otherwise
            // add `Content-Length: 0` to a 1xx / 204 / 304.
            return builder.build();
        }
        if (head.bodyLen == 0) {
            return builder.body(VesperaWireCodec.EMPTY_BODY);
        }
        return builder.body(new InputStreamResource(
                new ByteArrayInputStream(wire, head.bodyOff, head.bodyLen)));
    }

    private record EntityHead(HttpStatusCode status, HttpHeaders headers, int bodyOff, int bodyLen) {}

    private static EntityHead entityHead(byte[] wire, String method, ProxyServletResponse proxied) {
        int headerLen = VesperaWireCodec.readHeaderLength(wire);
        HttpHeaders httpHeaders = new HttpHeaders();
        HeaderPolicy.ResponseHeaderAccumulator acc =
                applyWireHeaderToHttpHeaders(wire, 4, headerLen, httpHeaders);
        HttpStatusCode status = HttpStatusCode.valueOf(acc.status);
        int bodyOff = 4 + headerLen;
        int bodyLen = wire.length - bodyOff;
        int bytesToExpose = responsePermitsBody(acc.status, method) ? bodyLen : 0;
        long contentLength = bufferedContentLength(
                acc.status, method, bodyLen, acc.declaredContentLength());
        if (contentLength >= 0) {
            httpHeaders.setContentLength(contentLength);
        }
        if (proxied != null) {
            if (acc.panicked) {
                proxied.markPanicked();
            }
            String idHeader = proxied.requestIdHeader();
            if (idHeader != null && httpHeaders.getFirst(idHeader) == null) {
                httpHeaders.set(idHeader, proxied.requestId());
            }
            httpHeaders = proxied.dedupeCorsHeaders(httpHeaders);
        }
        return new EntityHead(status, httpHeaders, bodyOff, bytesToExpose);
    }

    // ── Output adapters ───────────────────────────────────────────────────

    /** Drops body writes once the committed status/method forbids a body. */
    static final class BodyPermittingOutputStream extends OutputStream {
        private final OutputStream delegate;
        private final String method;
        private boolean permitsBody = true;

        BodyPermittingOutputStream(OutputStream delegate, String method) {
            this.delegate = Objects.requireNonNull(delegate, "delegate");
            this.method = method;
        }

        void applyPermitsBody(boolean permitsBody) {
            this.permitsBody = permitsBody && requestMethodPermitsBody(method);
        }

        @Override
        public void write(int b) throws IOException {
            if (permitsBody) {
                delegate.write(b);
            }
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (permitsBody) {
                delegate.write(b, off, len);
            }
        }

        @Override
        public void flush() throws IOException {
            delegate.flush();
        }
    }
}
