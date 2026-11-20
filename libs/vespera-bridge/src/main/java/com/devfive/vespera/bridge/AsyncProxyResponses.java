package com.devfive.vespera.bridge;

import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;
import org.springframework.web.context.request.async.DeferredResult;
import org.springframework.web.server.ResponseStatusException;

import java.io.IOException;
import java.util.Collections;
import java.util.IdentityHashMap;
import java.util.Set;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletionException;
import java.util.concurrent.RejectedExecutionException;

/**
 * The static {@code ASYNC} helpers of {@link VesperaProxyController}: the
 * {@link DeferredResult} with its timeout response, the failure → response
 * mapping, and the capped response-entity build. Package-private + static so
 * each is unit-testable without a live JNI dispatch.
 */
final class AsyncProxyResponses {

    private AsyncProxyResponses() {}

    /**
     * The ASYNC {@link DeferredResult} (Spring's default async timeout). Its
     * timeout result is the {@link ProxyErrors} plain-text {@code 503} with
     * the request id echoed: without one, Spring raises
     * {@code AsyncRequestTimeoutException}, which Spring Boot renders through
     * its {@code /error} page instead of the proxy's error shape.
     */
    static DeferredResult<ResponseEntity<?>> newAsyncResult(ProxyServletResponse response) {
        return new DeferredResult<>(null, () -> asyncTimeoutResponse(response));
    }

    static ResponseEntity<?> asyncTimeoutResponse(ProxyServletResponse response) {
        ResponseEntity<byte[]> entity = ProxyErrors.entity(
                HttpStatus.SERVICE_UNAVAILABLE.value(), "vespera: async dispatch timed out");
        String idHeader = response == null ? null : response.requestIdHeader();
        if (idHeader == null) {
            return entity;
        }
        // Builder only: HttpHeaders' copy methods differ between Spring 6 and 7.
        return ResponseEntity.status(entity.getStatusCode())
                .headers(entity.getHeaders())
                .header(idHeader, response.requestId())
                .body(entity.getBody());
    }

    /**
     * The status reported for a container-signalled async error
     * ({@code DeferredResult.onError}): a client that went away (an
     * {@link IOException} anywhere in the chain — Tomcat's
     * {@code ClientAbortException}, Spring's {@code AsyncRequestNotUsableException},
     * Jetty's {@code EofException}) is {@value VesperaReactiveProxyHandler#CLIENT_CLOSED_REQUEST}
     * (the WebFlux proxy's status for the same event, no WARN), anything else
     * {@code 500}.
     */
    static int asyncErrorStatus(Throwable failure) {
        return causeOfType(failure, IOException.class)
                ? VesperaReactiveProxyHandler.CLIENT_CLOSED_REQUEST : 500;
    }

    /**
     * Report a container-signalled async error once: a client disconnect as
     * {@code 499}, anything else as {@code 500}. Either way the failure never
     * carried a Rust answer (the container failed before the native result
     * was delivered), so the request is not "answered by a handler" and the
     * client-selected app stays out of the learned tags. A non-disconnect
     * failure is logged by the exchange's single WARN line, with its cause and
     * the request id; the response body stays generic.
     */
    static void reportAsyncError(ProxyExchange exchange, Throwable failure, boolean panicked) {
        exchange.reachedRust = false;
        int status = asyncErrorStatus(failure);
        exchange.complete(status, panicked, status == 499 ? null : failure);
    }

    /**
     * Map an async-dispatch failure to a {@link ProxyErrors} plain-text
     * response: a saturated-executor rejection becomes a
     * {@code 503 Service Unavailable} backpressure signal, a
     * {@link ResponseStatusException} (e.g. the buffered-response cap) its
     * status, anything else a {@code 500}. The request id is echoed like every
     * other mode's proxy errors.
     */
    static ResponseEntity<?> asyncFailureToResponse(Throwable ex, ProxyServletResponse proxied) {
        ResponseEntity<?> entity = asyncFailureToResponse(ex);
        String idHeader = proxied == null ? null : proxied.requestIdHeader();
        if (idHeader == null) {
            return entity;
        }
        // Builder only: HttpHeaders' copy methods differ between Spring 6 and 7.
        return ResponseEntity.status(entity.getStatusCode())
                .headers(entity.getHeaders())
                .header(idHeader, proxied.requestId())
                .body(entity.getBody());
    }

    /**
     * Settle the ASYNC result with the native outcome: {@code built} on
     * success, else the mapped failure. The result is set once, and only the
     * call that set it completes the exchange (a timeout or a container error
     * completed it already); a failure's cause rides along as the exchange's
     * one WARN line, never as a second log line of its own.
     */
    static void settle(DeferredResult<ResponseEntity<?>> result, ProxyExchange exchange,
            ProxyServletResponse response, ResponseEntity<?> built, Throwable failure) {
        ResponseEntity<?> entity = failure == null
                ? built : asyncFailureToResponse(failure, response);
        if (result.setResult(entity)) {
            exchange.complete(entity.getStatusCode().value(), response.panicked(),
                    failure == null ? null : unwrapCompletion(failure));
        }
    }

    static ResponseEntity<?> asyncFailureToResponse(Throwable ex) {
        if (isRejectedExecution(ex)) {
            return ProxyErrors.entity(HttpStatus.SERVICE_UNAVAILABLE.value(),
                    "vespera: async response executor saturated");
        }
        Throwable cause = unwrapCompletion(ex);
        if (cause instanceof CancellationException) {
            // The request already ended (timeout / disconnect) and cancelled
            // the native dispatch; this entity is never sent.
            return ProxyErrors.entity(HttpStatus.SERVICE_UNAVAILABLE.value(),
                    "vespera: dispatch cancelled");
        }
        if (cause instanceof ResponseStatusException rejected) {
            return ProxyErrors.entity(rejected.getStatusCode().value(), rejected.getReason());
        }
        return ProxyErrors.entity(500, ProxyErrors.describe(cause));
    }

    /** The failure behind a {@link CompletionException} wrapper (the throwable itself otherwise). */
    static Throwable unwrapCompletion(Throwable ex) {
        return ex instanceof CompletionException && ex.getCause() != null ? ex.getCause() : ex;
    }

    /** Whether {@code ex} (or any cause in its chain) is a rejected submission. */
    static boolean isRejectedExecution(Throwable ex) {
        return causeOfType(ex, RejectedExecutionException.class);
    }

    /**
     * Whether {@code ex} or any cause in its chain is a {@code type}. Each
     * throwable is visited once, so a cause cycle ({@code a.initCause(b)}
     * with {@code b} caused by {@code a} is legal) ends the walk instead of
     * spinning forever.
     */
    private static boolean causeOfType(Throwable ex, Class<? extends Throwable> type) {
        Set<Throwable> seen = Collections.newSetFromMap(new IdentityHashMap<>());
        for (Throwable t = ex; t != null && seen.add(t); t = t.getCause()) {
            if (type.isInstance(t)) {
                return true;
            }
        }
        return false;
    }

    /**
     * Build the buffered {@code ASYNC} response entity, enforcing the
     * {@code vespera.bridge.max-buffered-response-bytes} cap FIRST — parity with
     * the {@code SYNC} path. Without this a custom {@link DispatchModeResolver}
     * returning {@link DispatchMode#ASYNC} would heap-buffer an arbitrarily
     * large Rust response (retained until Spring finishes writing it),
     * defeating the cap and risking OOM / GC pressure. Runs on the async
     * response executor (NOT the Tokio completion thread), so the cap check
     * stays off the native worker. The body is a zero-copy view
     * ({@link WireResponseWriter#buildAsyncResponseEntityFromWire}).
     */
    static ResponseEntity<?> buildCappedAsyncResponseEntity(
            byte[] wire, String method, long maxBufferedResponseBytes,
            ProxyServletResponse response) {
        VesperaProxyController.rejectOversizedBufferedResponse(wire, maxBufferedResponseBytes);
        return WireResponseWriter.buildAsyncResponseEntityFromWire(wire, method, response);
    }
}
