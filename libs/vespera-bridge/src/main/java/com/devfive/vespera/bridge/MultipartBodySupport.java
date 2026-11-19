package com.devfive.vespera.bridge;

import jakarta.servlet.ServletInputStream;
import jakarta.servlet.http.HttpServletRequest;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.web.server.ResponseStatusException;

import java.io.InputStream;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Detects a {@code multipart/*} request body the servlet stack consumed
 * before the proxy could forward it.
 *
 * <p>A multipart body is parsed into temp-file parts by
 * {@code HttpServletRequest#getParts()} — called by a {@code MultipartResolver}
 * the {@link VesperaProxyMultipartResolver} does not wrap, Spring's
 * {@code MultipartFilter}, or any {@code getParameter()} call (the container
 * parses multipart bodies for request parameters too). The servlet stream is
 * then drained, and Rust would receive an empty body under the original
 * {@code Content-Type}.
 *
 * <p>Unlike a url-encoded form ({@link FormBodySupport}), the raw body cannot
 * be rebuilt faithfully: preamble, part-header spelling and order, transfer
 * encodings and parts a size limit rejected are lost, and re-serialising
 * large files would buffer them. So the proxy fails loudly instead — a
 * {@code 500} with {@link #CONSUMED_MSG} and a one-time WARN naming the fix —
 * rather than handing the Rust {@code Multipart} extractor an empty body.
 */
final class MultipartBodySupport {

    private static final Logger log = LoggerFactory.getLogger(MultipartBodySupport.class);

    /** Body of the proxy's {@code 500} for a consumed multipart body. */
    static final String CONSUMED_MSG = "vespera: the multipart request body was consumed before the proxy "
            + "could forward it (MultipartResolver / MultipartFilter / getParts() / getParameter())";

    private static final String MULTIPART = "multipart/";

    private static final AtomicBoolean WARNED = new AtomicBoolean();

    private MultipartBodySupport() {}

    /** Whether the request declares a {@code multipart/*} body. */
    static boolean isMultipart(HttpServletRequest request) {
        String contentType = request.getContentType();
        return contentType != null
                && contentType.strip().regionMatches(true, 0, MULTIPART, 0, MULTIPART.length());
    }

    /**
     * Buffered modes: reject a multipart {@code body} that came back empty
     * although the request declares one (or shorter than its
     * {@code Content-Length}).
     */
    static void rejectIfConsumed(HttpServletRequest request, RequestShape shape, byte[] body) {
        if (shape.definitelyBodyless || shape.contentLength == 0 || !isMultipart(request)
                || framedBodyless(request, shape)) {
            return;
        }
        boolean shortRead = body.length == 0
                || (shape.contentLength > 0 && body.length < shape.contentLength);
        if (shortRead) {
            throw consumed(request);
        }
    }

    /**
     * Bidirectional streaming: reject a multipart request whose servlet
     * stream is already finished before the proxy read a byte
     * ({@link ServletInputStream#isFinished()}: no blocking read, no
     * {@code 100-continue} side effect).
     */
    static void rejectIfConsumed(HttpServletRequest request, RequestShape shape, InputStream in) {
        if (shape.definitelyBodyless || shape.contentLength == 0 || !isMultipart(request)
                || framedBodyless(request, shape) || !(in instanceof ServletInputStream servlet)) {
            return;
        }
        boolean finished;
        try {
            finished = servlet.isFinished();
        } catch (RuntimeException unsupported) {
            // A wrapper that does not implement isFinished: cannot tell, forward as-is.
            return;
        }
        if (finished) {
            throw consumed(request);
        }
    }

    /**
     * A request with neither {@code Content-Length} nor {@code Transfer-Encoding}
     * has no body under HTTP/1.x framing, for every method (RFC 9112 6.3). An
     * empty body is then the real one and is forwarded as-is: a body the
     * container could have consumed must exist first, and the framing rules out
     * one. Only HTTP/1.x qualifies. HTTP/2 ends a body with END_STREAM, which
     * looks the same as a stream a container already drained, so that case keeps
     * the 500.
     */
    static boolean framedBodyless(HttpServletRequest request, RequestShape shape) {
        if (shape.contentLength >= 0 || shape.transferEncodingPresent) {
            return false;
        }
        String protocol = request.getProtocol();
        return protocol != null && protocol.regionMatches(true, 0, "HTTP/1.", 0, 7);
    }

    private static ResponseStatusException consumed(HttpServletRequest request) {
        String hint = "{} {}: multipart body was already consumed (parsed into parts) before the vespera "
                + "proxy ran, so it cannot be forwarded to Rust; answering 500. The autoconfigured "
                + "VesperaProxyMultipartResolver keeps the DispatcherServlet from parsing proxied requests "
                + "(vespera.bridge.multipart-passthrough, default true) - check for a MultipartFilter, a "
                + "filter calling getParts()/getParameter() (e.g. CsrfFilter without the token header) or a "
                + "@RequestMapping 'params' condition, and exclude the proxied paths from it.";
        if (WARNED.compareAndSet(false, true)) {
            log.warn(hint, request.getMethod(), request.getRequestURI());
        } else if (log.isDebugEnabled()) {
            log.debug(hint, request.getMethod(), request.getRequestURI());
        }
        return new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR, CONSUMED_MSG);
    }
}
