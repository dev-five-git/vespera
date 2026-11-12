package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletResponse;

import java.io.IOException;
import java.nio.ByteBuffer;

/**
 * The proxy's {@link DispatchMode#DIRECT} path: a pooled direct-buffer
 * dispatch plus its two overflow fallbacks (a response stashed on the Rust
 * side, and a safe-method re-run through response streaming), and the
 * adaptive {@link DirectOverflowMemory} that streams known-large routes up
 * front.
 *
 * <p>Only <em>safe</em> methods reach {@link #dispatch}: the controller
 * downgrades every other method to {@code SYNC} first, because an overflow
 * retry re-runs the Rust handler.
 */
final class DirectModeDispatcher {

    // Adaptive DIRECT-overflow avoidance: routes that overflowed the pooled
    // direct buffer once are streamed up front thereafter, removing the
    // repeated DIRECT-overflow-then-stream double dispatch a known-large
    // (download) route would otherwise pay on every request. Internal state,
    // so it is created directly rather than injected.
    private final DirectOverflowMemory overflowMemory = new DirectOverflowMemory();
    private final boolean retryOnOverflow;

    DirectModeDispatcher(boolean retryOnOverflow) {
        this.retryOnOverflow = retryOnOverflow;
    }

    /**
     * Whether a route that would take DIRECT overflowed before and should be
     * streamed up front instead.  A single volatile read until the first
     * overflow is recorded, so non-overflowing apps pay nothing.
     */
    boolean shouldAvoidDirect(String appName, String method, String path, String query) {
        return overflowMemory.shouldAvoidDirect(appName, method, path, query);
    }

    /**
     * "Safe" per RFC 9110 (GET/HEAD/OPTIONS) and retry enabled — the DIRECT
     * overflow retry is allowed even though the replayed response may differ
     * (timestamps, random IDs). Idempotent-but-unsafe methods (PUT/DELETE)
     * are excluded: their second run can return a different response
     * (e.g. DELETE → 204 then 404), so on overflow they fail with
     * {@link VesperaBridge.BufferTooSmallException} instead of silently
     * double-executing.
     */
    static boolean shouldRetry(boolean retryOnOverflow, String method) {
        return retryOnOverflow && HttpMethods.isSafe(method);
    }

    /**
     * Direct-buffer dispatch — the request body is materialised (DIRECT is
     * gated to small bounded payloads by the resolver) and the response is
     * served from the pooled direct buffer without a {@code byte[]}: the
     * header slice is decoded to commit status/headers, then the body region
     * is channelled straight into the servlet output stream.
     */
    void dispatch(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body,
            Boolean currentThreadIsVirtual) throws IOException {
        boolean retry = shouldRetry(retryOnOverflow, method);
        ByteBuffer wireResp;
        try {
            // Encodes straight into the pooled direct buffer — no
            // intermediate wire-sized byte[].
            wireResp = VesperaBridge.dispatchDirectPooled(
                    appName, method, path, query, headers, body, retry,
                    currentThreadIsVirtual == null
                            ? VesperaBridge.currentThreadIsVirtual()
                            : currentThreadIsVirtual.booleanValue());
        } catch (StashedOverflowException stashed) {
            // The finished response (an event stream, or larger than the
            // pooled buffer cap) is still held on the Rust side: stream it
            // instead of re-running the handler, and stream this route up
            // front next time.
            overflowMemory.recordOverflow(appName, method, path, query);
            WireResponseWriter.BodyPermittingOutputStream bodyOut =
                    new WireResponseWriter.BodyPermittingOutputStream(response.getOutputStream(), method);
            stashed.streamTo(
                    headerBytes -> bodyOut.applyPermitsBody(
                            WireResponseWriter.applyDecodedHeader(headerBytes, response, method)),
                    bodyOut);
            response.getOutputStream().flush();
            return;
        } catch (VesperaBridge.BufferTooSmallException overflow) {
            // The first dispatch already ran; its oversized result was discarded.
            if (retry) {
                // Safe method + retry enabled: the response is larger than the
                // pooled direct buffer's hard cap. Re-route through response
                // streaming so a large download streams chunk-by-chunk instead
                // of being heap-buffered (a dispatchBytes fallback could spike
                // the JVM heap on multi-GiB responses). A safe re-run is not
                // intended to mutate state, but its response may differ
                // (timestamps, random IDs). The DIRECT path has not committed
                // yet, so streaming takes over cleanly.
                //
                // Remember this route so the NEXT request to it streams up
                // front — recorded only here, where we actually fall back.
                overflowMemory.recordOverflow(appName, method, path, query);
                streamResponse(response, appName, method, path, query, headers, body);
                return;
            }
            // Retry disabled: re-running could return a different response,
            // so surface the size to the operator instead of double-executing.
            ProxyErrors.write(response, 500, "vespera DIRECT overflow: response needs "
                    + overflow.requiredSize()
                    + " bytes; route this request via BIDIRECTIONAL_STREAMING");
            return;
        }

        // Commit status + headers parsed straight from the direct buffer —
        // no byte[] copy, no DecodedResponse object graph. addHeader on the
        // still-uncommitted response is equivalent to setHeader for a
        // header's first value and appends for multi-valued headers
        // (e.g. set-cookie).
        int bodyLen = WireResponseWriter.applyDirectHeaderAndPositionBody(wireResp, response, method);

        // Stream the body region of the direct buffer with an explicit
        // per-thread heap scratch (Channels.newChannel(OutputStream) would
        // allocate its own temporary heap buffer per write).
        if (bodyLen > 0) {
            VesperaDirectBufferPool.writeDirectBody(wireResp, response.getOutputStream());
        }
    }

    /**
     * Response-only streaming — request body materialised, response
     * streams chunk-by-chunk to the servlet output stream.  Status
     * and headers commit through the JNI header callback BEFORE the
     * first body byte hits the wire.  The {@code STREAMING} mode and the
     * DIRECT overflow fallback.
     */
    static void streamResponse(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body) throws IOException {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        WireResponseWriter.BodyPermittingOutputStream bodyOut =
                new WireResponseWriter.BodyPermittingOutputStream(response.getOutputStream(), method);
        VesperaBridge.dispatchStreamingWithHeader(
                wireReq,
                headerBytes -> bodyOut.applyPermitsBody(
                        WireResponseWriter.applyDecodedHeader(headerBytes, response, method)),
                bodyOut);
        response.getOutputStream().flush();
    }
}
