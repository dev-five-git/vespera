package com.devfive.vespera.bridge;

import org.springframework.core.io.buffer.DataBuffer;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.server.reactive.ServerHttpResponse;
import reactor.core.publisher.Mono;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;

/**
 * The WebFlux response side of {@link VesperaReactiveProxyHandler} — the
 * reactive twin of {@link ProxyServletResponse} + {@link WireResponseWriter}:
 * applies a Rust wire response header (status, {@link HeaderPolicy}-filtered
 * headers, proxy-owned {@code Content-Length}), echoes the request id unless
 * Rust set that header, de-duplicates {@code Access-Control-*} headers per
 * {@link CorsHandling}, remembers a caught Rust panic, and writes the
 * proxy's own plain-text errors ({@link ProxyErrors} shape).
 *
 * <p>Only {@link HttpHeaders} methods present in Spring Framework 6.2 and 7
 * are used ({@code getFirst} / {@code set} / {@code add} /
 * {@code setContentLength}). Package-private: an internal collaborator of the
 * reactive proxy.
 */
final class ReactiveProxyResponse {
    private static final String CORS_PREFIX = "access-control-";

    final ServerHttpResponse response;
    private final String requestIdHeader;
    private final String requestId;
    private final CorsHandling cors;
    /** Lower-case names of headers taken from the Rust response. */
    private final Set<String> written = new HashSet<>(16);
    private boolean upstreamRequestId;
    /** Status the proxy set (Rust's or its own), for observers. */
    private volatile int status = 200;
    private volatile boolean panicked;
    /** Whether a Rust wire header was applied (the response is Rust's from then on). */
    private volatile boolean headerApplied;
    /** Headers as they were before a streamed Rust header was applied ({@link #discardRustOutput}). */
    private HttpHeaders beforeRust;

    ReactiveProxyResponse(ServerHttpResponse response, String requestIdHeader, String requestId,
            CorsHandling cors) {
        this.response = response;
        this.requestIdHeader = requestId == null ? null : requestIdHeader;
        this.requestId = requestId;
        this.cors = cors;
    }

    int status() {
        return status;
    }

    boolean panicked() {
        return panicked;
    }

    boolean headerApplied() {
        return headerApplied;
    }

    /** Flag the response as a caught Rust panic (a streamed body that panicked after its header). */
    void markPanicked() {
        panicked = true;
    }

    /** Pre-set the request-id echo; Rust's own value (if any) replaces it later. */
    void applyDefaultRequestId() {
        if (requestIdHeader != null && !upstreamRequestId) {
            response.getHeaders().set(requestIdHeader, requestId);
        }
    }

    /**
     * Apply the wire header at {@code wire[off, off+len)}: status and filtered
     * headers. Returns the accumulator (status, declared content length).
     */
    HeaderPolicy.ResponseHeaderAccumulator applyWireHeader(byte[] wire, int off, int len) {
        return applyWireHeader(wire, off, len, false);
    }

    /**
     * {@link #applyWireHeader(byte[], int, int)}; {@code resettable} first
     * snapshots the current headers so {@link #discardRustOutput} restores
     * them exactly (a streamed body can still fail before the commit).
     */
    HeaderPolicy.ResponseHeaderAccumulator applyWireHeader(byte[] wire, int off, int len,
            boolean resettable) {
        if (resettable) {
            beforeRust = copy(response.getHeaders(), null);
        }
        HeaderPolicy.ResponseHeaderAccumulator acc = new HeaderPolicy.ResponseHeaderAccumulator();
        acc.status = WireHeaderReader.apply(wire, off, len, acc);
        response.setStatusCode(HttpStatusCode.valueOf(acc.status));
        status = acc.status;
        for (HeaderPolicy.HeaderPair header : acc.headers) {
            String name = header.name();
            if (!HeaderPolicy.isHopByHopResponseHeader(name)
                    && !HeaderPolicy.isContentLengthHeader(name)
                    && !HeaderPolicy.isConnectionNominatedHeader(name, acc.connectionTokens)) {
                addUpstreamHeader(name, header.value());
            }
        }
        if (acc.panicked) {
            panicked = true;
        }
        headerApplied = true;
        return acc;
    }

    /** Add one Rust response header, honouring the request-id echo and CORS ownership. */
    void addUpstreamHeader(String name, String value) {
        HttpHeaders headers = response.getHeaders();
        String lower = name.toLowerCase(Locale.ROOT);
        if (requestIdHeader != null && requestIdHeader.equalsIgnoreCase(name) && !upstreamRequestId) {
            // The first upstream request-id value replaces the pre-set echo.
            upstreamRequestId = true;
            headers.set(name, value);
            written.add(lower);
            return;
        }
        if (cors != null && lower.startsWith(CORS_PREFIX)
                && !written.contains(lower) && headers.getFirst(name) != null) {
            // Spring (a CorsWebFilter, Spring Security cors()) or another filter
            // already set this header.
            if (cors == CorsHandling.SPRING) {
                return; // Spring owns CORS: keep its value, drop Rust's.
            }
            headers.set(name, value); // Rust owns CORS: its first value replaces.
            written.add(lower);
            return;
        }
        headers.add(name, value);
        written.add(lower);
    }

    /**
     * Undo the applied Rust header on the still-uncommitted response, for a
     * proxy-written error: restore the headers from before it (filter-set
     * ones such as CORS, security headers and the request-id echo survive;
     * Rust's {@code Set-Cookie}, {@code Cache-Control}, ... are dropped).
     * Without a snapshot, the headers Rust wrote are removed by name.
     */
    void discardRustOutput() {
        if (!headerApplied) {
            return;
        }
        HttpHeaders headers = response.getHeaders();
        HttpHeaders restored = beforeRust != null ? beforeRust : copy(headers, written);
        headers.clear();
        restored.forEach(headers::addAll);
        beforeRust = null;
        written.clear();
        if (upstreamRequestId) {
            upstreamRequestId = false;
            applyDefaultRequestId();
        }
        headerApplied = false;
    }

    /** A copy of {@code headers}, minus the lower-case names in {@code skip} (if any). */
    private static HttpHeaders copy(HttpHeaders headers, Set<String> skip) {
        HttpHeaders copy = new HttpHeaders();
        headers.forEach((name, values) -> {
            if (skip == null || !skip.contains(name.toLowerCase(Locale.ROOT))) {
                copy.addAll(name, List.copyOf(values));
            }
        });
        return copy;
    }

    /** Set the proxy-owned {@code Content-Length} ({@code < 0} = leave unset). */
    void setContentLength(long length) {
        if (length >= 0) {
            response.getHeaders().setContentLength(length);
        }
    }

    /** Write {@code wire[off, off+len)} as the whole body (no copy), or just complete when empty. */
    Mono<Void> writeBody(byte[] wire, int off, int len) {
        if (len <= 0) {
            return response.setComplete();
        }
        DataBuffer buffer = response.bufferFactory().wrap(ByteBuffer.wrap(wire, off, len));
        return response.writeWith(Mono.just(buffer));
    }

    /**
     * Write a proxy-side error ({@link ProxyErrors} shape: status +
     * {@code text/plain; charset=utf-8} message). The caller checks that the
     * response is not committed yet.
     */
    Mono<Void> writeError(int errorStatus, String message) {
        byte[] body = (message == null ? "" : message).getBytes(StandardCharsets.UTF_8);
        status = errorStatus;
        response.setStatusCode(HttpStatusCode.valueOf(errorStatus));
        HttpHeaders headers = response.getHeaders();
        headers.set(HttpHeaders.CONTENT_TYPE, ProxyErrors.CONTENT_TYPE);
        headers.setContentLength(body.length);
        return response.writeWith(Mono.just(response.bufferFactory().wrap(body)));
    }
}
