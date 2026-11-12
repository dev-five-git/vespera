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

    /**
     * Replace whatever the uncommitted {@code response} holds with the error.
     * Returns {@code false} (writing nothing) once the response is committed.
     */
    static boolean write(HttpServletResponse response, int status, String message)
            throws IOException {
        if (response.isCommitted()) {
            return false;
        }
        response.reset();
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

    /** Message for an unexpected failure: the exception message, or its type. */
    static String describe(Throwable failure) {
        String message = failure.getMessage();
        return "vespera dispatch failed: "
                + (message == null || message.isBlank() ? failure.getClass().getName() : message);
    }

    private static byte[] bytes(String message) {
        return (message == null ? "" : message).getBytes(StandardCharsets.UTF_8);
    }
}
