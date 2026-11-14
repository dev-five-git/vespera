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
     * Body of the WebFlux proxy {@code 503} when every streaming thread is busy
     * and the streaming queue is full ({@link VesperaStreamingExecutor}).
     * A compile-time constant, so the reactive proxy never loads this
     * (servlet-dependent) class.
     */
    static final String STREAMING_SATURATED = "vespera streaming capacity exhausted";

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
        return write(response, status, message, false);
    }

    /** Body of the proxy {@code 500} for a Rust panic (same as the native panic wire). */
    static final String RUST_PANIC = "panic in Rust engine";

    /** Body of the proxy {@code 500} for a response body that failed mid-stream. */
    static final String STREAM_ABORTED = "response body stream error";

    /**
     * A header-first streaming dispatch failed after the Rust header was
     * applied to {@code response} ({@code aborted}, normally a
     * {@link VesperaStreamAbortedException}): while nothing reached the client
     * yet, replace the (possibly {@code 200}) status, Rust headers and
     * buffered partial body with a proxy {@code 500} — flagged panicked for a
     * Rust panic, exactly like the buffered modes — instead of leaving the
     * outcome to the servlet container's error page.  Returns {@code false}
     * once the response is committed: the caller must rethrow so the
     * container aborts the connection (the client sees a truncated
     * transfer, never a cleanly terminated short body).
     *
     * @param closeConnection also send {@code Connection: close} (a detached
     *                        request-body read leaves the stream position unknown)
     */
    static boolean writeStreamAbort(HttpServletResponse response, IOException aborted,
            boolean closeConnection) throws IOException {
        boolean panic = VesperaStreamAbortedException.isPanic(aborted);
        if (panic) {
            ProxyServletResponse.markPanicked(response);
        }
        return write(response, 500, panic ? RUST_PANIC : STREAM_ABORTED, closeConnection);
    }

    private static boolean write(HttpServletResponse response, int status, String message,
            boolean closeConnection) throws IOException {
        if (response.isCommitted()) {
            return false;
        }
        if (response instanceof ProxyServletResponse proxied) {
            proxied.resetProxyOutput();
        } else {
            response.resetBuffer();
        }
        if (closeConnection) {
            response.setHeader("Connection", "close");
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
