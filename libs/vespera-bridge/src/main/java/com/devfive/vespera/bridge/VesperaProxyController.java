package com.devfive.vespera.bridge;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
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
import java.nio.charset.StandardCharsets;
import java.util.Enumeration;
import java.util.LinkedHashMap;
import java.util.List;
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
 * 1.0.0) keep the proxy transparent for every payload size while
 * routing small bounded idempotent requests through the
 * direct-buffer fast path (DIRECT 2.2 µs / SYNC 3.2 µs vs streaming
 * 24.1 µs on a small {@code GET /health}).  Restore the pre-1.0.0
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
                return dispatchSync(appName, method, path, query, headers,
                        readBody(request));
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

    private static byte[] readBody(HttpServletRequest request) throws IOException {
        try (InputStream in = request.getInputStream()) {
            return in.readAllBytes();
        }
    }

    private ResponseEntity<?> dispatchSync(
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        byte[] wireResp = VesperaBridge.dispatchBytes(wireReq);
        DecodedResponse decoded = VesperaBridge.decodeResponse(wireResp);
        return buildResponseEntity(decoded);
    }

    private CompletableFuture<ResponseEntity<?>> dispatchAsyncFlow(
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        return VesperaBridge.dispatch(wireReq).thenApply(wireResp -> {
            DecodedResponse decoded = VesperaBridge.decodeResponse(wireResp);
            return buildResponseEntity(decoded);
        });
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

        // Commit status + headers from the wire header slice (small copy).
        int headerLen = wireResp.getInt(0);
        byte[] headerWire = new byte[4 + headerLen];
        wireResp.get(0, headerWire);
        applyDecodedHeader(headerWire, response);

        // Stream the body region of the direct buffer straight out.
        wireResp.position(4 + headerLen);
        if (wireResp.hasRemaining()) {
            Channels.newChannel(response.getOutputStream()).write(wireResp);
        }
        response.getOutputStream().flush();
    }

    /** Idempotent per RFC 9110 — safe to re-run on DIRECT overflow retry. */
    private static boolean isIdempotent(String method) {
        return switch (method == null ? "" : method.toUpperCase(Locale.ROOT)) {
            case "GET", "HEAD", "PUT", "DELETE", "OPTIONS" -> true;
            default -> false;
        };
    }

    private static Map<String, String> collectHeaders(HttpServletRequest request) {
        Map<String, String> headers = new LinkedHashMap<>();
        Enumeration<String> names = request.getHeaderNames();
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            headers.put(name.toLowerCase(Locale.ROOT), request.getHeader(name));
        }
        return headers;
    }

    /**
     * Apply a decoded wire header to {@link HttpServletResponse} —
     * called from streaming dispatch callbacks BEFORE the first body
     * byte is written, while the response is still uncommitted.
     */
    private static void applyDecodedHeader(byte[] headerBytes,
                                            HttpServletResponse response) {
        DecodedResponse meta = VesperaBridge.decodeResponse(headerBytes);
        response.setStatus(meta.status());
        for (Map.Entry<String, Object> entry : meta.headers().entrySet()) {
            Object val = entry.getValue();
            if (val instanceof List<?> list) {
                for (Object v : list) {
                    response.addHeader(entry.getKey(), String.valueOf(v));
                }
            } else if (val != null) {
                response.setHeader(entry.getKey(), String.valueOf(val));
            }
        }
    }

    /**
     * Convert a fully-decoded sync/async wire response into a
     * Spring {@link ResponseEntity}.  Body is delivered as
     * {@link String} for text-like Content-Types,
     * {@code byte[]} otherwise.
     */
    private static ResponseEntity<?> buildResponseEntity(DecodedResponse decoded) {
        HttpHeaders httpHeaders = new HttpHeaders();
        for (Map.Entry<String, Object> entry : decoded.headers().entrySet()) {
            Object val = entry.getValue();
            if (val instanceof List<?> list) {
                for (Object v : list) {
                    httpHeaders.add(entry.getKey(), String.valueOf(v));
                }
            } else if (val != null) {
                httpHeaders.set(entry.getKey(), String.valueOf(val));
            }
        }
        HttpStatus status = HttpStatus.valueOf(decoded.status());
        String contentType = httpHeaders.getFirst(HttpHeaders.CONTENT_TYPE);
        if (isTextContentType(contentType)) {
            String bodyStr = new String(decoded.bodyBytes(), StandardCharsets.UTF_8);
            return new ResponseEntity<>(bodyStr, httpHeaders, status);
        }
        return new ResponseEntity<>(decoded.bodyBytes(), httpHeaders, status);
    }

    private static boolean isTextContentType(String ct) {
        if (ct == null) return true;
        int parameterStart = ct.indexOf(';');
        String mediaType = parameterStart >= 0 ? ct.substring(0, parameterStart) : ct;
        String mime = mediaType.trim().toLowerCase(Locale.ROOT);
        return mime.startsWith("text/")
                || mime.equals("application/json")
                || mime.endsWith("+json")
                || mime.equals("application/xml")
                || mime.endsWith("+xml")
                || mime.equals("application/javascript")
                || mime.equals("application/ecmascript")
                || mime.equals("application/yaml")
                || mime.equals("application/x-yaml")
                || mime.equals("application/x-www-form-urlencoded")
                || mime.equals("application/graphql");
    }
}
