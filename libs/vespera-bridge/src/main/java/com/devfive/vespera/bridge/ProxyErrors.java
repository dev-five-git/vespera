package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletResponse;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

import java.io.IOException;
import java.nio.charset.StandardCharsets;

/**
 * Errors the proxy itself produces (oversized buffered bodies, unknown
 * apps, native failures, DIRECT overflow, async backpressure) are written in
 * the same shape as the Rust {@code error_wire}: the status plus a
 * {@code text/plain; charset=utf-8} body holding the message — never routed
 * through Spring's {@code /error} page, whose JSON / whitelabel shape depends
 * on the client's {@code Accept}.
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyErrors {
    static final String CONTENT_TYPE = "text/plain; charset=utf-8";
    private static final MediaType MEDIA_TYPE = MediaType.parseMediaType(CONTENT_TYPE);

    private ProxyErrors() {}

    /** Body of a proxy {@code 500} for an unexpected failure; details go to the WARN log only. */
    static final String GENERIC_FAILURE = "vespera dispatch failed";

    /** Body of the proxy {@code 503} when the native library was never loaded. */
    static final String NOT_INITIALIZED = "vespera native library is not initialized";

    /**
     * Replace what the proxy wrote into the uncommitted {@code response} with
     * the error: the buffered body is discarded ({@code resetBuffer()}) and,
     * for a {@link ProxyServletResponse}, only the headers the proxy itself
     * set (Rust's) are dropped — headers set by servlet filters before the
     * proxy ran (CORS, security headers, cookies) are kept.  Returns
     * {@code false} (writing nothing) once the response is committed.
     */
    static boolean write(HttpServletResponse response, int status, String message)
            throws IOException {
        if (response.isCommitted()) {
            return false;
        }
        if (response instanceof ProxyServletResponse proxied) {
            proxied.resetProxyOutput();
        } else {
            response.resetBuffer();
        }
        byte[] body = bytes(message);
        response.setStatus(status);
        response.setContentType(CONTENT_TYPE);
        response.setContentLength(body.length);
        response.getOutputStream().write(body);
        response.getOutputStream().flush();
        return true;
    }

    /** The same error as a {@link ResponseEntity} (ASYNC completion). */
    static ResponseEntity<byte[]> entity(int status, String message) {
        return ResponseEntity.status(HttpStatusCode.valueOf(status))
                .contentType(MEDIA_TYPE)
                .body(bytes(message));
    }

    /**
     * Client-facing message for an unexpected failure: always
     * {@link #GENERIC_FAILURE} — exception messages can carry internals
     * (paths, hosts, SQL) and are logged at WARN with the request id instead.
     */
    static String describe(Throwable failure) {
        return GENERIC_FAILURE;
    }

    private static byte[] bytes(String message) {
        return (message == null ? "" : message).getBytes(StandardCharsets.UTF_8);
    }
}
