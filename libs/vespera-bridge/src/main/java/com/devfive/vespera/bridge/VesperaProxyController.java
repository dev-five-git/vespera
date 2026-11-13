package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.slf4j.MDC;
import org.springframework.http.HttpStatus;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.context.request.async.DeferredResult;
import org.springframework.web.server.ResponseStatusException;

import java.io.IOException;
import java.io.InputStream;
import java.util.List;
import java.util.Locale;
import java.util.Objects;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.Executor;
import java.util.concurrent.ForkJoinPool;
import java.util.concurrent.RejectedExecutionException;

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
 *   <li>{@link PrincipalResolver#resolvePrincipal(HttpServletRequest)}
 *       — the trusted authenticated principal forwarded in the wire
 *       header's {@code "principal"} field (never client headers).</li>
 * </ol>
 *
 * <p>The autoconfigured defaults ({@link HeaderAppNameResolver} on
 * {@code X-Vespera-App} + {@link SmartDispatchModeResolver} since
 * 0.2.0) keep the proxy transparent for every payload size while
 * routing small bounded safe requests through the
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
    private final PrincipalResolver principalResolver;
    private final Executor asyncResponseExecutor;
    private final DirectModeDispatcher direct;
    private final long maxBufferedRequestBytes;
    private final long maxBufferedResponseBytes;
    /** Lower-case app-selection header stripped before forwarding, or {@code null}. */
    private final String appSelectionHeader;
    /** Request-id header (as configured), or {@code null} when request ids are disabled. */
    private final String requestIdHeader;
    /** {@link #requestIdHeader} lower-cased, as forwarded to Rust. */
    private final String requestIdHeaderLower;
    private final List<DispatchObserver> observers;
    /** Bounded app names for logs / metrics / {@link DispatchEvent}s. */
    private final AppTags appTags;
    /** {@code Access-Control-*} de-duplication mode, or {@code null} (none). */
    private final CorsHandling cors;

    /** Body of the proxy's own {@code 404} for an app outside {@code exposed-apps}. */
    static final String UNKNOWN_APP_MSG = "vespera app is not exposed by this proxy";

    /** SLF4J MDC key holding the request id while a request is dispatched. */
    public static final String MDC_REQUEST_ID = "requestId";

    static final long DEFAULT_MAX_BUFFERED_REQUEST_BYTES = 64L * 1024L * 1024L;
    static final long DEFAULT_MAX_BUFFERED_RESPONSE_BYTES = 64L * 1024L * 1024L;

    /**
     * One-time guard for the "custom resolver routed an UNSAFE method to
     * DIRECT, downgraded to SYNC" warning. A misconfigured custom
     * {@link DispatchModeResolver} would otherwise log on every unsafe
     * request; warn once at WARN, then DEBUG thereafter, so the
     * misconfiguration is observable without per-request log spam.
     */
    private static final java.util.concurrent.atomic.AtomicBoolean
            UNSAFE_DIRECT_DOWNGRADE_WARNED =
                    new java.util.concurrent.atomic.AtomicBoolean(false);

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver) {
        this(appResolver, modeResolver, ForkJoinPool.commonPool(), true,
                DEFAULT_MAX_BUFFERED_REQUEST_BYTES, DEFAULT_MAX_BUFFERED_RESPONSE_BYTES);
    }

    public VesperaProxyController(AppNameResolver appResolver,
                                    DispatchModeResolver modeResolver,
                                    Executor asyncResponseExecutor,
                                    boolean directRetryOnOverflow) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow,
                DEFAULT_MAX_BUFFERED_REQUEST_BYTES, DEFAULT_MAX_BUFFERED_RESPONSE_BYTES);
    }

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                   Executor asyncResponseExecutor,
                                   boolean directRetryOnOverflow,
                                   long maxBufferedRequestBytes) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow,
                maxBufferedRequestBytes, DEFAULT_MAX_BUFFERED_RESPONSE_BYTES);
    }

    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                  Executor asyncResponseExecutor,
                                  boolean directRetryOnOverflow,
                                  long maxBufferedRequestBytes,
                                  long maxBufferedResponseBytes) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow,
                maxBufferedRequestBytes, maxBufferedResponseBytes, null, List.of());
    }

    /**
     * Constructor with request ids and observers.
     *
     * @param requestIdHeader request-id header to read / generate / forward /
     *                        echo ({@code vespera.bridge.request-id.header}),
     *                        or {@code null} to disable request ids
     * @param observers       {@link DispatchObserver}s notified after every
     *                        proxied request
     */
    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                  Executor asyncResponseExecutor,
                                  boolean directRetryOnOverflow,
                                  long maxBufferedRequestBytes,
                                  long maxBufferedResponseBytes,
                                  String requestIdHeader,
                                  List<DispatchObserver> observers) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow,
                maxBufferedRequestBytes, maxBufferedResponseBytes, requestIdHeader, observers, null);
    }

    /**
     * Constructor with CORS handling; forwards no principal.
     *
     * @param cors who owns CORS ({@code vespera.bridge.cors}); decides how
     *             Rust {@code Access-Control-*} response headers are merged
     *             with ones Spring already set, or {@code null} to pass them
     *             through unchanged
     */
    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                  Executor asyncResponseExecutor,
                                  boolean directRetryOnOverflow,
                                  long maxBufferedRequestBytes,
                                  long maxBufferedResponseBytes,
                                  String requestIdHeader,
                                  List<DispatchObserver> observers,
                                  CorsHandling cors) {
        this(appResolver, modeResolver, asyncResponseExecutor, directRetryOnOverflow,
                maxBufferedRequestBytes, maxBufferedResponseBytes, requestIdHeader, observers, cors,
                null);
    }

    /**
     * Full constructor (used by the autoconfiguration).
     *
     * @param principalResolver trusted principal forwarded to Rust
     *                          ({@code vespera.bridge.forward-principal}), or
     *                          {@code null} to forward none
     */
    public VesperaProxyController(AppNameResolver appResolver,
                                  DispatchModeResolver modeResolver,
                                  Executor asyncResponseExecutor,
                                  boolean directRetryOnOverflow,
                                  long maxBufferedRequestBytes,
                                  long maxBufferedResponseBytes,
                                  String requestIdHeader,
                                  List<DispatchObserver> observers,
                                  CorsHandling cors,
                                  PrincipalResolver principalResolver) {
        this.principalResolver = principalResolver == null ? PrincipalResolver.none() : principalResolver;
        this.cors = cors;
        this.requestIdHeader = requestIdHeader == null || requestIdHeader.isBlank()
                ? null : requestIdHeader.trim();
        this.requestIdHeaderLower = this.requestIdHeader == null
                ? null : this.requestIdHeader.toLowerCase(Locale.ROOT);
        this.observers = observers == null ? List.of() : List.copyOf(observers);
        this.appResolver = Objects.requireNonNull(appResolver, "appResolver");
        this.modeResolver = Objects.requireNonNull(modeResolver, "modeResolver");
        this.asyncResponseExecutor = Objects.requireNonNull(asyncResponseExecutor, "asyncResponseExecutor");
        this.direct = new DirectModeDispatcher(directRetryOnOverflow);
        this.maxBufferedRequestBytes = Math.max(0, maxBufferedRequestBytes);
        this.maxBufferedResponseBytes = Math.max(0, maxBufferedResponseBytes);
        this.appSelectionHeader = appResolver.selectionHeaderName();
        this.appTags = AppTags.forResolver(appResolver);
    }

    @RequestMapping(value = "/**", consumes = MediaType.ALL_VALUE)
    public Object proxy(HttpServletRequest request,
                        HttpServletResponse servletResponse) throws IOException {
        final String requestId = requestIdHeader == null
                ? null : resolveRequestId(request.getHeader(requestIdHeader));
        final ProxyServletResponse response =
                new ProxyServletResponse(servletResponse, requestIdHeader, requestId, cors);
        final ProxyExchange exchange =
                new ProxyExchange(log, observers, appTags, request.getMethod(), requestId);
        if (requestId != null) {
            MDC.put(MDC_REQUEST_ID, requestId);
        }
        try {
            Object result = dispatch(request, response, exchange);
            if (result == null) {
                exchange.complete(response.getStatus(), response.panicked());
            }
            return result;
        } catch (RuntimeException failure) {
            failWith(response, exchange, failure);
            return null;
        } catch (UnsatisfiedLinkError unlinked) {
            // A native symbol is missing: the library was never loaded (or
            // predates this bridge). Answer 503 instead of escaping as an Error.
            log.warn("vespera native call failed for {} {} requestId={}",
                    exchange.method, exchange.path, requestId, unlinked);
            failWith(response, exchange, new ResponseStatusException(
                    HttpStatus.SERVICE_UNAVAILABLE, ProxyErrors.NOT_INITIALIZED));
            return null;
        } catch (IOException failure) {
            exchange.complete(response.isCommitted() ? response.getStatus() : 500,
                    response.panicked());
            throw failure;
        } finally {
            if (requestId != null) {
                MDC.remove(MDC_REQUEST_ID);
            }
        }
    }

    /**
     * Write a proxy-side failure ({@link ResponseStatusException} -> its
     * status, anything else -> {@code 500}) as a {@link ProxyErrors} plain-text
     * response; a failure after the response was committed cannot be
     * rewritten and is rethrown.
     */
    private static void failWith(
            ProxyServletResponse response, ProxyExchange exchange, RuntimeException failure)
            throws IOException {
        int status;
        String message;
        if (failure instanceof ResponseStatusException rejected) {
            status = rejected.getStatusCode().value();
            message = rejected.getReason();
        } else {
            // Details (message, stack) stay in the log; the client gets a
            // generic body plus the request id header to correlate.
            log.warn("vespera dispatch of {} {} failed requestId={}",
                    exchange.method, exchange.path, exchange.requestId, failure);
            status = 500;
            message = ProxyErrors.describe(failure);
        }
        response.applyDefaultRequestId();
        if (!ProxyErrors.write(response, status, message)) {
            exchange.complete(response.getStatus(), response.panicked());
            throw failure;
        }
        exchange.complete(status, response.panicked());
    }

    /**
     * The client's request id when it is a sane token (1-128 visible ASCII
     * characters), else a fresh random UUID.
     */
    static String resolveRequestId(String incoming) {
        return RequestIds.resolve(incoming);
    }

    static boolean isValidRequestId(String value) {
        return RequestIds.isValid(value);
    }

    /** The routing + dispatch body of {@link #proxy}; {@code null} once the response is written. */
    private Object dispatch(HttpServletRequest request, ProxyServletResponse response,
            ProxyExchange exchange) throws IOException {
        final RequestShape shape = RequestShape.capture(request);
        final String appName = resolveAppName(request, exchange);
        final Boolean currentThreadIsVirtual;
        final DispatchMode mode;
        if (modeResolver instanceof SmartDispatchModeResolver smartResolver) {
            boolean virtualThread = VesperaBridge.currentThreadIsVirtual();
            currentThreadIsVirtual = Boolean.valueOf(virtualThread);
            mode = smartResolver.resolveMode(request, virtualThread);
        } else {
            currentThreadIsVirtual = null;
            mode = modeResolver.resolveMode(request);
        }
        final String method = shape.method;
        // Path RELATIVE to the servlet context: a Spring app deployed under
        // a non-root context (e.g. server.servlet.context-path=/api) must
        // still forward `/health` — not `/api/health` — so the Rust router
        // sees exactly the URL published in the generated openapi.json.
        final String path = pathWithinApplication(request);
        exchange.app = appName;
        exchange.path = path;
        final String query = Objects.toString(request.getQueryString(), "");
        // Adaptive DIRECT-overflow avoidance: a route that overflowed the
        // pooled direct buffer before is streamed up front, so it dispatches
        // ONCE instead of paying the DIRECT-overflow-then-stream double
        // dispatch again. `shouldAvoidDirect` is a single volatile read until
        // the first overflow is recorded, so non-overflowing apps pay nothing.
        final DispatchMode effectiveMode = formSafeMode(effectiveMode(
                mode,
                mode == DispatchMode.DIRECT
                        && direct.shouldAvoidDirect(appName, method, path, query)),
                request);

        exchange.mode = effectiveMode;
        if (!VesperaBridge.isInitialized()) {
            // No native library: every native call would throw
            // UnsatisfiedLinkError. Fail cleanly before touching the body.
            throw new ResponseStatusException(
                    HttpStatus.SERVICE_UNAVAILABLE, ProxyErrors.NOT_INITIALIZED);
        }
        if (log.isDebugEnabled()) {
            log.debug("-> Rust  {} {} app={} mode={}", method, path, appName, effectiveMode);
        }
        if (effectiveMode != DispatchMode.ASYNC) {
            // ASYNC answers with a ResponseEntity; its builder adds the id.
            response.applyDefaultRequestId();
        }

        // For bidirectional streaming, pass the servlet InputStream
        // straight through — DO NOT pre-read it.  For every other
        // mode, materialise the body bytes here (replaces Spring's
        // @RequestBody, which we cannot use because it would consume
        // the InputStream and leave the bidirectional path empty).
        if (effectiveMode == DispatchMode.BIDIRECTIONAL_STREAMING) {
            exchange.reachedRust = true;
            dispatchBidirectional(request, response, appName, method, path, query,
                    forwardedHeaders(request, null, exchange));
            return null;
        }
        // Every other mode materialises the REQUEST body (STREAMING only
        // streams the response), so all of them honour the same
        // vespera.bridge.max-buffered-request-bytes cap.
        final byte[] body = readBody(request, shape, maxBufferedRequestBytes);
        final VesperaBridge.HeaderSource headers = forwardedHeaders(
                request, forwardedContentLength(shape, body), exchange);
        exchange.reachedRust = true;
        switch (effectiveMode) {
            case SYNC:
                dispatchSync(response, appName, method, path, query, headers, body,
                        maxBufferedResponseBytes);
                return null;
            case ASYNC:
                return dispatchAsyncFlow(appName, method, path, query, headers, body,
                        response, exchange);
            case STREAMING:
                DirectModeDispatcher.streamResponse(response, appName, method, path, query, headers, body);
                return null;
            case DIRECT:
            default:
                dispatchDirectMode(response, appName, method, path, query, headers, body,
                        currentThreadIsVirtual);
                return null;
        }
    }

    /**
     * Resolve the target app; an {@link UnknownAppException} (an app outside
     * {@code vespera.bridge.exposed-apps}) becomes {@code 404} before anything
     * is dispatched.
     */
    private String resolveAppName(HttpServletRequest request, ProxyExchange exchange) {
        try {
            return appResolver.resolveAppName(request);
        } catch (UnknownAppException unknown) {
            // Reported as AppTags.UNKNOWN (never verbatim: client-controlled).
            exchange.app = unknown.appName();
            // Fixed body: the app name is client-controlled, so the 404 neither
            // echoes it nor tells exposed and unexposed-but-registered apps apart.
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, UNKNOWN_APP_MSG);
        }
    }

    /**
     * A form-urlencoded body may already have been consumed into request
     * parameters by the servlet stack (see {@link FormBodySupport}); the
     * bidirectional path would forward that drained stream as an empty body,
     * so such requests buffer the (small) request body and stream only the
     * response.
     */
    static DispatchMode formSafeMode(DispatchMode mode, HttpServletRequest request) {
        return mode == DispatchMode.BIDIRECTIONAL_STREAMING && FormBodySupport.isFormUrlEncoded(request)
                ? DispatchMode.STREAMING
                : mode;
    }

    /**
     * The {@code content-length} to forward when the buffered body differs
     * from the declared length (a consumed form body rebuilt from parameters),
     * else {@code null} to forward the client's header unchanged.
     */
    static String forwardedContentLength(RequestShape shape, byte[] body) {
        return shape.contentLength >= 0 && body.length != shape.contentLength
                ? Integer.toString(body.length)
                : null;
    }

    /**
     * Request headers forwarded to Rust: the {@link HeaderPolicy} filter,
     * minus the app-selection header ({@code excludedHeader}, lower-case, may be
     * {@code null}), with {@code content-length} replaced when
     * {@code contentLength} is non-null.
     */
    static VesperaBridge.HeaderSource requestHeaders(
            HttpServletRequest request, String excludedHeader, String contentLength) {
        return new ProxyHeaderSource(request, excludedHeader, contentLength, null, null, null);
    }

    /**
     * {@link #requestHeaders} plus this controller's request-id header, the
     * client address and the {@link PrincipalResolver}'s trusted principal.
     */
    private VesperaBridge.HeaderSource forwardedHeaders(
            HttpServletRequest request, String contentLength, ProxyExchange exchange) {
        return new ProxyHeaderSource(request, appSelectionHeader, contentLength,
                requestIdHeaderLower, exchange.requestId, principalResolver.resolvePrincipal(request));
    }

    /**
     * Resolve the request path RELATIVE to the application so a Spring app
     * deployed under a non-root context
     * ({@code server.servlet.context-path=/api}) or behind a prefix-mapped
     * {@code DispatcherServlet} ({@code spring.mvc.servlet.path=/v1}) still
     * forwards the URL the Rust router and the generated
     * {@code openapi.json} know — {@code /api/health} on the wire becomes
     * {@code /health}; {@code ;jsessionid=...} path parameters are dropped.
     * A request to the bare application root collapses to {@code "/"} (see
     * {@link RequestPaths}).
     *
     * <p>Package-private so unit tests can verify it directly with
     * {@code MockHttpServletRequest}.
     */
    static String pathWithinApplication(HttpServletRequest request) {
        return RequestPaths.pathWithinApplication(request);
    }

    static DispatchMode effectiveMode(DispatchMode mode, boolean avoidDirect) {
        return mode == DispatchMode.DIRECT && avoidDirect ? DispatchMode.STREAMING : mode;
    }

    /**
     * Largest body for which {@link #readBody} trusts {@code
     * Content-Length} enough to pre-allocate the exact array.  Beyond
     * this (or for unknown length) it falls back to {@code readAllBytes},
     * which grows with the bytes actually present — so a lying / huge
     * {@code Content-Length} header cannot force a giant up-front
     * allocation.
     */
    private static final int MAX_FIXED_BODY = 64 * 1024 * 1024;

    /**
     * Largest body that can be materialised into a single Java {@code byte[]}
     * (the JVM array-length ceiling is just under {@link Integer#MAX_VALUE}).
     * A buffered request whose length provably exceeds this can never be read
     * via {@code readAllBytes}/{@code readNBytes}, so it is rejected with 413
     * rather than allowed to attempt an impossible allocation; such requests
     * must go through {@code BIDIRECTIONAL_STREAMING}.
     */
    private static final long MAX_BUFFERED_BODY = Integer.MAX_VALUE - 8L;

    /**
     * 413 as a raw status code: {@code HttpStatus.CONTENT_TOO_LARGE} only exists
     * in Spring Framework 7 (6.x calls it {@code PAYLOAD_TOO_LARGE}), so naming
     * either constant would break the other supported Spring Boot line.
     */
    private static final HttpStatusCode CONTENT_TOO_LARGE = HttpStatusCode.valueOf(413);

    // Package-private (not private) so unit tests can exercise the
    // bodyless fast path and length-based reads with MockHttpServletRequest.
    static byte[] readBody(HttpServletRequest request) throws IOException {
        return readBody(request, 0);
    }

    static byte[] readBody(HttpServletRequest request, long maxBufferedRequestBytes)
            throws IOException {
        return readBody(request, RequestShape.from(request), maxBufferedRequestBytes);
    }

    static byte[] readBody(
            HttpServletRequest request, RequestShape shape, long maxBufferedRequestBytes)
            throws IOException {
        // Provably bodyless requests skip the servlet InputStream
        // acquisition + readAllBytes allocations entirely. This covers
        // both Content-Length: 0 AND length-less GET/HEAD/OPTIONS (the
        // hottest path — the small safe GETs the SmartDispatch
        // resolver routes through DIRECT, which previously still paid a
        // getInputStream()+readAllBytes() round-trip on an empty body).
        if (shape.definitelyBodyless) {
            return VesperaWireCodec.EMPTY_BODY;
        }
        long contentLength = shape.contentLength;
        long cap = Math.max(0, maxBufferedRequestBytes);
        if (cap > 0 && contentLength > cap) {
            throw payloadTooLarge(contentLength, cap);
        }
        // A buffered body must fit a single Java byte[] (≈ 2 GiB). A larger
        // known Content-Length can never be materialised here, so reject it
        // (413) instead of letting readAllBytes()/readNBytes() attempt an
        // impossible allocation and throw OutOfMemoryError. Such requests must
        // go through BIDIRECTIONAL_STREAMING.
        if (contentLength > MAX_BUFFERED_BODY) {
            throw payloadTooLarge(contentLength, MAX_BUFFERED_BODY);
        }
        byte[] body;
        try (InputStream in = request.getInputStream()) {
            body = readBufferedBody(in, contentLength, cap);
        }
        // Spring's FormContentFilter / an earlier getParameter() call may have
        // consumed a form-urlencoded body into request parameters already.
        return FormBodySupport.recoverConsumedBody(request, body, contentLength);
    }

    static byte[] readBufferedBody(InputStream in, long contentLength, long cap)
            throws IOException {
        if (cap > 0 && contentLength < 0) {
            long cappedPlusOne = cap == Long.MAX_VALUE ? Long.MAX_VALUE : cap + 1;
            long effectiveLimit = Math.min(cappedPlusOne, MAX_BUFFERED_BODY);
            int readLimit = (int) effectiveLimit;
            byte[] body = in.readNBytes(readLimit);
            if ((long) body.length > cap) {
                throw payloadTooLarge(body.length, cap);
            }
            rejectAtBufferedBodyLimit(body.length, cap);
            return body;
        }
        if (contentLength > 0 && (cap > 0 || contentLength <= MAX_FIXED_BODY)) {
            // Known, bounded length: one exact allocation filled in
            // place, skipping readAllBytes()'s grow-by-doubling and
            // its final trim copy.  readNBytes blocks until the
            // buffer is full or EOF; the servlet container caps the
            // stream at Content-Length, so a well-formed request
            // returns exactly contentLength bytes (a short read
            // yields a correctly-sized smaller array).
            return in.readNBytes((int) contentLength);
        }
        // Unknown length (-1) with NO soft cap: a buffered mode is the
        // WRONG path for an open-ended stream (it should have been routed
        // to BIDIRECTIONAL_STREAMING). Bound the read at MAX_FIXED_BODY
        // (64 MiB) instead of the ~2 GiB single-array ceiling, so a
        // (mis)configured resolver feeding a runaway chunked upload into a
        // buffered mode cannot grow the JVM heap toward OOM. Reading one
        // byte past the bound distinguishes "exactly at the bound" from
        // "over". Known-length bodies keep the documented `cap=0`
        // "unlimited" behaviour below.
        if (contentLength < 0) {
            byte[] body = in.readNBytes(MAX_FIXED_BODY + 1);
            rejectAboveDefaultBufferedLimit(body.length);
            return body;
        }
        // Oversized KNOWN length with no explicit cap (cap=0,
        // contentLength > MAX_FIXED_BODY): the caller opted into unlimited
        // buffering for a SIZED body, so honour it up to the single-array
        // ceiling (the actual read stops at the known Content-Length).
        byte[] body = in.readNBytes((int) MAX_BUFFERED_BODY);
        rejectAtBufferedBodyLimit(body.length, MAX_BUFFERED_BODY);
        return body;
    }

    static void rejectAtBufferedBodyLimit(long bodyLength, long cap) {
        if (bodyLength == MAX_BUFFERED_BODY && cap >= MAX_BUFFERED_BODY) {
            throw payloadTooLarge(bodyLength, MAX_BUFFERED_BODY);
        }
    }

    static void rejectAboveDefaultBufferedLimit(long bodyLength) {
        if (bodyLength > MAX_FIXED_BODY) {
            throw payloadTooLarge(bodyLength, MAX_FIXED_BODY);
        }
    }

    private static ResponseStatusException payloadTooLarge(long actualBytes, long capBytes) {
        return new ResponseStatusException(
                CONTENT_TOO_LARGE,
                "buffered request body exceeds vespera.bridge.max-buffered-request-bytes="
                        + capBytes + " (actual " + actualBytes + " bytes)");
    }

    /**
     * Synchronous dispatch — writes the wire response straight to the
     * servlet response (status + headers via {@link WireHeaderReader},
     * then the body region written directly from the wire array).  This
     * drops both the body-sized {@code Arrays.copyOfRange} and the
     * {@code ResponseEntity<byte[]>} object that the prior
     * {@code buildResponseEntityFromWire} path allocated per response.
     * Mirrors {@link #dispatchDirectMode}; the async path still uses
     * {@code buildResponseEntityFromWire} (Spring async completion), but
     * returns a zero-copy {@code Resource} view over the wire body.
     */
    static void dispatchSync(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body) throws IOException {
        dispatchSync(response, appName, method, path, query, headers, body, 0);
    }

    private static void dispatchSync(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body,
            long maxBufferedResponseBytes) throws IOException {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        // Parks (instead of pinning the carrier) on a virtual thread.
        byte[] wireResp = ParkingDispatch.dispatchBytes(wireReq, VesperaBridge.currentThreadIsVirtual());
        rejectOversizedBufferedResponse(wireResp, javaResponseCap(maxBufferedResponseBytes));
        WireResponseWriter.writeWireResponse(wireResp, response, method);
    }

    /**
     * The cap the Java side must still enforce after a buffered dispatch:
     * {@code 0} (none) when the native buffered-response cap is known to be
     * at least as strict — the autoconfiguration forwards
     * {@code vespera.bridge.max-buffered-response-bytes} to the native side,
     * which then stops collecting the body before it is allocated and answers
     * {@code 500} itself — else {@code cap} as a fallback check (a controller
     * built by hand, or a native cap that could not be aligned).
     */
    static long javaResponseCap(long cap) {
        return VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.enforcesAtMost(cap) ? 0 : cap;
    }

    /**
     * Fallback buffered-response cap: {@code 500} (a server-side limit on
     * what the handler produced — the same status the native cap answers),
     * never the request-size status {@code 413}.
     */
    private static void rejectOversizedBufferedResponse(byte[] wireResp, long maxBufferedResponseBytes) {
        long cap = Math.max(0, maxBufferedResponseBytes);
        if (cap <= 0) {
            return;
        }
        int headerLen = VesperaWireCodec.readHeaderLength(wireResp);
        long bodyLen = (long) wireResp.length - 4L - headerLen;
        if (bodyLen > cap) {
            throw new ResponseStatusException(
                    HttpStatus.INTERNAL_SERVER_ERROR,
                    "buffered response body exceeds vespera.bridge.max-buffered-response-bytes="
                            + cap + " (actual " + bodyLen + " bytes); use streaming dispatch");
        }
    }


    /**
     * ASYNC dispatch: the native future completes on a Tokio worker, the
     * response entity is built on {@link #asyncResponseExecutor} (with the
     * request id in the MDC), and the outcome is reported once — by the
     * entity, or by Spring's async timeout ({@code 503}) when the request
     * timed out first.
     *
     * <p>Cancel-on-disconnect: when the servlet async request ends before the
     * native result (Spring async timeout, or a container-reported error such
     * as the client disconnecting), the native future is cancelled, which
     * aborts the Rust handler and frees its in-flight slot instead of letting
     * it run to completion for a response nobody can receive.
     */
    private DeferredResult<ResponseEntity<?>> dispatchAsyncFlow(
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body,
            ProxyServletResponse response, ProxyExchange exchange) {
        byte[] wireReq = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        DeferredResult<ResponseEntity<?>> result = new DeferredResult<>();
        final String requestId = exchange.requestId;
        final long cap = javaResponseCap(maxBufferedResponseBytes);
        CompletableFuture<byte[]> nativeResult = VesperaBridge.dispatch(wireReq);
        // Spring answers an async timeout with 503 (AsyncRequestTimeoutException).
        result.onTimeout(() -> {
            nativeResult.cancel(false);
            exchange.complete(HttpStatus.SERVICE_UNAVAILABLE.value(), response.panicked());
        });
        result.onError(failure -> {
            nativeResult.cancel(false);
            exchange.complete(500, response.panicked());
        });
        // Any other end of the async request (a no-op once the result is in).
        result.onCompletion(() -> nativeResult.cancel(false));
        nativeResult
                .thenApplyAsync(
                        wireResp -> ProxyExchange.withMdc(requestId,
                                () -> buildCappedAsyncResponseEntity(wireResp, method, cap, response)),
                        asyncResponseExecutor)
                // The async executor uses AbortPolicy (NOT CallerRunsPolicy):
                // under saturation the heavy wire response build must NOT run on
                // the thread that completed the native future — that is a Rust
                // Tokio worker, and stealing it degrades native dispatch
                // throughput (the documented "no heavy continuations on Tokio
                // workers" contract). A rejected submission surfaces here as a
                // RejectedExecutionException; translate it to a clean 503
                // backpressure signal instead of an opaque 500.
                .handle((resp, ex) -> ex == null ? resp
                        : ProxyExchange.withMdc(requestId, () -> asyncFailureToResponse(ex)))
                .thenAccept(entity -> {
                    if (result.setResult(entity)) {
                        exchange.complete(entity.getStatusCode().value(), response.panicked());
                    }
                });
        return result;
    }

    /**
     * Map an async-dispatch failure to a {@link ProxyErrors} plain-text
     * response: a saturated-executor rejection becomes a
     * {@code 503 Service Unavailable} backpressure signal, a
     * {@link ResponseStatusException} (e.g. the buffered-response cap) its
     * status, anything else a {@code 500}.  Package-private + static so the
     * classification ({@link #isRejectedExecution}) is unit-testable without
     * a live JNI dispatch.
     */
    static ResponseEntity<?> asyncFailureToResponse(Throwable ex) {
        if (isRejectedExecution(ex)) {
            return ProxyErrors.entity(HttpStatus.SERVICE_UNAVAILABLE.value(),
                    "vespera: async response executor saturated");
        }
        Throwable cause = ex instanceof CompletionException && ex.getCause() != null
                ? ex.getCause() : ex;
        if (cause instanceof CancellationException) {
            // The request already ended (timeout / disconnect) and cancelled
            // the native dispatch; this entity is never sent.
            return ProxyErrors.entity(HttpStatus.SERVICE_UNAVAILABLE.value(),
                    "vespera: dispatch cancelled");
        }
        if (cause instanceof ResponseStatusException rejected) {
            return ProxyErrors.entity(rejected.getStatusCode().value(), rejected.getReason());
        }
        log.warn("vespera async dispatch failed", cause);
        return ProxyErrors.entity(500, ProxyErrors.describe(cause));
    }

    /** Whether {@code ex} (or any cause in its chain) is a rejected submission. */
    static boolean isRejectedExecution(Throwable ex) {
        for (Throwable t = ex; t != null; t = t.getCause()) {
            if (t instanceof RejectedExecutionException) {
                return true;
            }
            if (t == t.getCause()) {
                break;
            }
        }
        return false;
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
            VesperaBridge.HeaderSource headers) throws IOException {
        byte[] wireHeader = VesperaBridge.encodeRequestHeader(
                appName, method, path, query, headers);
        WireResponseWriter.BodyPermittingOutputStream bodyOut =
                new WireResponseWriter.BodyPermittingOutputStream(response.getOutputStream(), method);
        // The Rust request producer can be detached while still parked in
        // read() (bounded join); the gate keeps it off the servlet stream
        // once the dispatch returned.
        GatedRequestInputStream requestBody = new GatedRequestInputStream(request.getInputStream());
        DirectModeDispatcher.HeaderFirstStream stream = () -> VesperaBridge.dispatchFullStreamingWithHeader(
                wireHeader,
                headerBytes -> bodyOut.applyPermitsBody(
                        WireResponseWriter.applyDecodedHeader(headerBytes, response, method)),
                requestBody,
                bodyOut);
        IOException aborted = null;
        boolean detached;
        try {
            stream.run();
        } catch (IOException failure) {
            // Failed after the Rust header was applied (panic, body error):
            // answered below once the request body is detached.
            aborted = failure;
        } finally {
            detached = detachRequestBody(requestBody, response);
        }
        if (aborted != null) {
            if (!ProxyErrors.writeStreamAbort(response, aborted, detached)) {
                throw aborted;
            }
            return;
        }
        response.getOutputStream().flush();
    }

    /**
     * Close the request-body gate after the native dispatch returned.  When
     * a detached producer is still inside a servlet {@code read}, the stream
     * position is unknown, so the connection must not be reused: send
     * {@code Connection: close} if the response is not committed yet.  If it
     * already is, this is best-effort only — the container sees the
     * in-flight read finish on its own (the gate returns {@code -1} to every
     * later read, so the producer never touches the servlet stream again).
     * Returns whether a read was still in flight.
     */
    static boolean detachRequestBody(
            GatedRequestInputStream requestBody, HttpServletResponse response) {
        if (!requestBody.detach()) {
            return false;
        }
        if (!response.isCommitted()) {
            response.setHeader("Connection", "close");
        } else {
            log.debug("request-body read still in flight after a committed bidirectional "
                    + "response; the connection may not be closed proactively");
        }
        return true;
    }

    /**
     * {@link DispatchMode#DIRECT} boundary: a <em>safe</em> method goes to
     * {@link DirectModeDispatcher}; every other method is served via
     * {@code SYNC}, because a DIRECT overflow retry re-runs the handler
     * (e.g. DELETE → 204 then 404), so the controller never double-executes
     * a handler whose response could change.
     */
    void dispatchDirectMode(
            HttpServletResponse response,
            String appName, String method, String path, String query,
            VesperaBridge.HeaderSource headers, byte[] body,
            Boolean currentThreadIsVirtual) throws IOException {
        if (!HttpMethods.isSafe(method)) {
            // DIRECT runs the Rust handler on the FIRST dispatch before any
            // overflow is known; for an UNSAFE method an overflow would 500
            // *after* the side effect already happened (partial unsafe
            // execution).  A custom DispatchModeResolver can route an unsafe
            // method here, so gate it at the controller boundary: serve unsafe
            // requests via SYNC, which never re-runs the handler.
            //
            // The autoconfigured SmartDispatchModeResolver never routes unsafe
            // methods to DIRECT, so reaching here means a CUSTOM resolver is
            // misconfigured. Make it observable: warn once (then DEBUG) so the
            // operator sees the silent downgrade without per-request spam.
            if (UNSAFE_DIRECT_DOWNGRADE_WARNED.compareAndSet(false, true)) {
                log.warn("DispatchModeResolver routed unsafe method {} to DIRECT; "
                        + "downgrading to SYNC (DIRECT overflow retry re-runs the handler, "
                        + "unsafe for non-safe methods). Fix the custom resolver to avoid "
                        + "this downgrade. Further occurrences log at DEBUG.", method);
            } else if (log.isDebugEnabled()) {
                log.debug("DispatchModeResolver routed unsafe method {} to DIRECT; "
                        + "downgrading to SYNC.", method);
            }
            dispatchSync(response, appName, method, path, query, headers, body,
                    maxBufferedResponseBytes);
            return;
        }
        direct.dispatch(response, appName, method, path, query, headers, body,
                currentThreadIsVirtual);
    }

    /**
     * Build the buffered {@code ASYNC} response entity, enforcing the
     * {@code vespera.bridge.max-buffered-response-bytes} cap FIRST — parity with
     * the {@code SYNC} path ({@link #dispatchSync} via
     * {@link #rejectOversizedBufferedResponse}). Without this a custom
     * {@link DispatchModeResolver} returning {@link DispatchMode#ASYNC} would
     * heap-buffer an arbitrarily large Rust response (retained until Spring finishes writing it), defeating the
     * cap and risking OOM / GC pressure. Runs on the async response executor
     * (NOT the Tokio completion thread), so the cap check stays off the native
     * worker. Package-private + static so the cap wiring is unit-testable
     * without a live JNI dispatch.
     */
    static ResponseEntity<?> buildCappedResponseEntityFromWire(
            byte[] wire, String method, long maxBufferedResponseBytes) {
        return buildCappedResponseEntityFromWire(wire, method, maxBufferedResponseBytes, null);
    }

    static ResponseEntity<?> buildCappedResponseEntityFromWire(
            byte[] wire, String method, long maxBufferedResponseBytes,
            ProxyServletResponse response) {
        rejectOversizedBufferedResponse(wire, maxBufferedResponseBytes);
        return WireResponseWriter.buildResponseEntityFromWire(wire, method, response);
    }

    /** As {@link #buildCappedResponseEntityFromWire}, with a zero-copy body view. */
    static ResponseEntity<?> buildCappedAsyncResponseEntity(
            byte[] wire, String method, long maxBufferedResponseBytes,
            ProxyServletResponse response) {
        rejectOversizedBufferedResponse(wire, maxBufferedResponseBytes);
        return WireResponseWriter.buildAsyncResponseEntityFromWire(wire, method, response);
    }

}
