package com.devfive.vespera.bridge;

import org.reactivestreams.Publisher;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.slf4j.MDC;
import org.springframework.core.io.buffer.DataBuffer;
import org.springframework.core.io.buffer.DataBufferLimitException;
import org.springframework.core.io.buffer.DataBufferUtils;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.server.reactive.ServerHttpRequest;
import org.springframework.http.server.reactive.ServerHttpResponse;
import org.springframework.web.server.ResponseStatusException;
import org.springframework.web.server.ServerWebExchange;
import org.springframework.web.server.WebHandler;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;
import reactor.core.scheduler.Scheduler;
import reactor.core.scheduler.Schedulers;

import java.io.IOException;
import java.io.OutputStream;
import java.util.List;
import java.util.Locale;
import java.util.Objects;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import java.util.function.Consumer;

/**
 * Catch-all Spring WebFlux proxy — the reactive counterpart of
 * {@link VesperaProxyController}, autoconfigured by
 * {@link VesperaBridgeReactiveAutoConfiguration} in a
 * {@code REACTIVE} web application (gated by
 * {@code vespera.bridge.controller-enabled}, default {@code true}) and mapped
 * by {@link VesperaReactiveHandlerMapping} to
 * {@code vespera.bridge.include-patterns} minus {@code exclude-patterns}.
 *
 * <p>Every URL published in vespera's generated {@code openapi.json} is
 * reachable at the same URL (relative to {@code spring.webflux.base-path}).
 * Per request the handler
 *
 * <ol>
 *   <li>resolves the target app ({@link ReactiveAppNameResolver}; an
 *       {@link UnknownAppException} answers {@code 404}) and the trusted
 *       principal ({@link ReactivePrincipalResolver});</li>
 *   <li>aggregates the request body without blocking, capped by
 *       {@code vespera.bridge.max-buffered-request-bytes} ({@code 413} when
 *       exceeded);</li>
 *   <li>dispatches with {@link VesperaBridge#dispatch(byte[])} — the
 *       non-blocking {@code dispatchAsync} path; nothing ever blocks a Netty
 *       event loop. When the client disconnects, the Reactor subscription is
 *       cancelled and the dispatch future with it, which aborts the Rust
 *       handler ({@code cancel-on-disconnect}) and frees its
 *       {@code vespera.async.maxInFlight} slot;</li>
 *   <li>writes the buffered response (zero-copy view of the wire body),
 *       capped by {@code vespera.bridge.max-buffered-response-bytes}.</li>
 * </ol>
 *
 * <p><strong>Streaming responses:</strong> a non-{@code HEAD} request whose
 * {@code Accept} names a streaming media type ({@code text/event-stream},
 * {@code application/x-ndjson}, {@code application/stream+json}) is served
 * by the native response-streaming symbol
 * ({@link VesperaBridge#dispatchStreamingWithHeader}) on Reactor's
 * bounded-elastic scheduler, bridged with backpressure through
 * {@link DataBufferUtils#outputStreamPublisher}: each stream occupies one
 * bounded-elastic thread while it runs, and a client disconnect stops the
 * Rust stream at its next write.
 *
 * <p>Proxy-side errors use the same plain-text shape as the servlet proxy
 * ({@link ProxyErrors}); request ids, {@link DispatchObserver}s (Micrometer)
 * and {@link CorsHandling} behave as in {@link VesperaProxyController}. A
 * request cancelled by the client is reported to observers with status
 * {@value #CLIENT_CLOSED_REQUEST}.
 */
public class VesperaReactiveProxyHandler implements WebHandler {

    private static final Logger log = LoggerFactory.getLogger(VesperaReactiveProxyHandler.class);

    /** Status reported to observers for a request the client cancelled (nginx convention). */
    public static final int CLIENT_CLOSED_REQUEST = 499;

    /** Size of the body chunks a streamed response is split into. */
    static final int STREAM_CHUNK_BYTES = 64 * 1024;

    /** Unknown-length body bound when no cap is configured (servlet parity). */
    private static final long MAX_FIXED_BODY = 64L * 1024L * 1024L;
    /** Largest body a single Java array holds. */
    private static final long MAX_BUFFERED_BODY = Integer.MAX_VALUE - 8L;

    /** Native seam: the real {@link VesperaBridge} natives, or a test fake. */
    interface Transport {
        boolean isInitialized();

        /** Non-blocking dispatch; the future always completes with a wire response. */
        CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest);

        /** Blocking response-streaming dispatch with a header-first callback. */
        void dispatchStreaming(byte[] wireRequest, Consumer<byte[]> headerConsumer, OutputStream body)
                throws IOException;
    }

    static final Transport NATIVE = new Transport() {
        @Override
        public boolean isInitialized() {
            return VesperaBridge.isInitialized();
        }

        @Override
        public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
            try {
                return VesperaBridge.dispatch(wireRequest);
            } catch (UnsatisfiedLinkError unlinked) {
                throw notInitialized(unlinked);
            }
        }

        @Override
        public void dispatchStreaming(byte[] wireRequest, Consumer<byte[]> headerConsumer, OutputStream body) {
            try {
                VesperaBridge.dispatchStreamingWithHeader(wireRequest, headerConsumer, body);
            } catch (UnsatisfiedLinkError unlinked) {
                throw notInitialized(unlinked);
            }
        }
    };

    private final ReactiveAppNameResolver appResolver;
    private final ReactivePrincipalResolver principalResolver;
    private final long maxBufferedRequestBytes;
    private final long maxBufferedResponseBytes;
    /** Request-id header as configured, or {@code null} when disabled. */
    private final String requestIdHeader;
    /** {@link #requestIdHeader} lower-cased, as forwarded to Rust. */
    private final String requestIdHeaderLower;
    private final List<DispatchObserver> observers;
    private final CorsHandling cors;
    private final String appSelectionHeader;
    private final AppTags appTags;
    private final Transport transport;
    /** Where the buffered response is written once the native future completed (off the Tokio worker). */
    private final Scheduler completionScheduler;
    /** Where the blocking native streaming dispatch runs. */
    private final Scheduler streamingScheduler;

    /** Handler with the defaults: no principal, 64 MiB buffered caps, no request ids. */
    public VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver) {
        this(appResolver, null, VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
                VesperaProxyController.DEFAULT_MAX_BUFFERED_RESPONSE_BYTES, null, List.of(), null);
    }

    /**
     * Full constructor (used by the autoconfiguration).
     *
     * @param principalResolver       trusted principal forwarded to Rust, or {@code null} for none
     * @param maxBufferedRequestBytes request body cap ({@code 413}); {@code <= 0} = only the
     *                                single-array bound (64 MiB for an unknown length)
     * @param maxBufferedResponseBytes response body cap ({@code 500}); {@code <= 0} = none
     * @param requestIdHeader         request-id header, or {@code null} to disable request ids
     * @param observers               notified after every proxied request
     * @param cors                    who owns CORS, or {@code null} to pass Rust's headers through
     */
    public VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver,
                                       ReactivePrincipalResolver principalResolver,
                                       long maxBufferedRequestBytes,
                                       long maxBufferedResponseBytes,
                                       String requestIdHeader,
                                       List<DispatchObserver> observers,
                                       CorsHandling cors) {
        this(appResolver, principalResolver, maxBufferedRequestBytes, maxBufferedResponseBytes,
                requestIdHeader, observers, cors, NATIVE, Schedulers.parallel(), Schedulers.boundedElastic());
    }

    VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver,
                                ReactivePrincipalResolver principalResolver,
                                long maxBufferedRequestBytes,
                                long maxBufferedResponseBytes,
                                String requestIdHeader,
                                List<DispatchObserver> observers,
                                CorsHandling cors,
                                Transport transport,
                                Scheduler completionScheduler,
                                Scheduler streamingScheduler) {
        this.appResolver = Objects.requireNonNull(appResolver, "appResolver");
        this.principalResolver = principalResolver == null ? ReactivePrincipalResolver.none() : principalResolver;
        this.maxBufferedRequestBytes = Math.max(0, maxBufferedRequestBytes);
        this.maxBufferedResponseBytes = Math.max(0, maxBufferedResponseBytes);
        this.requestIdHeader = requestIdHeader == null || requestIdHeader.isBlank()
                ? null : requestIdHeader.trim();
        this.requestIdHeaderLower = this.requestIdHeader == null
                ? null : this.requestIdHeader.toLowerCase(Locale.ROOT);
        this.observers = observers == null ? List.of() : List.copyOf(observers);
        this.cors = cors;
        this.appSelectionHeader = appResolver.selectionHeaderName();
        this.appTags = new AppTags(appResolver instanceof ReactiveHeaderAppNameResolver header
                ? header.exposedApps() : null);
        this.transport = Objects.requireNonNull(transport, "transport");
        this.completionScheduler = Objects.requireNonNull(completionScheduler, "completionScheduler");
        this.streamingScheduler = Objects.requireNonNull(streamingScheduler, "streamingScheduler");
    }

    @Override
    public Mono<Void> handle(ServerWebExchange exchange) {
        ServerHttpRequest request = exchange.getRequest();
        String requestId = requestIdHeader == null
                ? null : RequestIds.resolve(request.getHeaders().getFirst(requestIdHeader));
        String method = request.getMethod().name();
        ProxyExchange px = new ProxyExchange(log, observers, appTags, method, requestId);
        ReactiveProxyResponse out = new ReactiveProxyResponse(
                exchange.getResponse(), requestIdHeader, requestId, cors);
        px.path = pathWithinApplication(request);
        out.applyDefaultRequestId();
        Mono<Void> flow;
        if (requestId != null) {
            MDC.put(VesperaProxyController.MDC_REQUEST_ID, requestId);
        }
        try {
            flow = dispatch(exchange, out, px, method);
        } catch (RuntimeException failure) {
            flow = Mono.error(failure);
        } finally {
            if (requestId != null) {
                MDC.remove(VesperaProxyController.MDC_REQUEST_ID);
            }
        }
        return flow
                .onErrorResume(failure -> fail(out, px, failure))
                .doOnCancel(() -> {
                    // The client went away. Not "answered by a handler": keep
                    // the client-selected app name out of the learned tags.
                    px.reachedRust = false;
                    px.complete(CLIENT_CLOSED_REQUEST, out.panicked());
                });
    }

    /** Resolve, read the body, dispatch; throws synchronously for early rejections. */
    private Mono<Void> dispatch(ServerWebExchange exchange, ReactiveProxyResponse out,
            ProxyExchange px, String method) {
        ServerHttpRequest request = exchange.getRequest();
        final String appName = resolveAppName(exchange, px);
        HttpHeaders requestHeaders = request.getHeaders();
        final boolean streaming = !"HEAD".equals(method)
                && SmartDispatchModeResolver.acceptsStreaming(requestHeaders.getFirst(HttpHeaders.ACCEPT));
        px.mode = streaming ? DispatchMode.STREAMING : DispatchMode.ASYNC;
        if (!transport.isInitialized()) {
            throw new ResponseStatusException(HttpStatus.SERVICE_UNAVAILABLE, ProxyErrors.NOT_INITIALIZED);
        }
        if (log.isDebugEnabled()) {
            log.debug("-> Rust  {} {} app={} mode={}", method, px.path, appName, px.mode);
        }
        long declared = requestHeaders.getContentLength();
        long cap = maxBufferedRequestBytes;
        if (cap > 0 && declared > cap) {
            throw payloadTooLarge(declared, cap);
        }
        if (declared > MAX_BUFFERED_BODY) {
            throw payloadTooLarge(declared, MAX_BUFFERED_BODY);
        }
        String rawQuery = request.getURI().getRawQuery();
        final String query = rawQuery == null ? "" : rawQuery;
        Mono<Optional<VesperaPrincipal>> principal = Mono.defer(() -> {
            Mono<VesperaPrincipal> resolved = principalResolver.resolvePrincipal(exchange);
            return resolved == null ? Mono.empty() : resolved;
        }).map(Optional::of).defaultIfEmpty(Optional.empty());
        // A WebFilter (Spring Security's CsrfWebFilter) may have consumed a
        // form body through exchange.getFormData(): rebuild it from that.
        Mono<byte[]> body = readBody(request, declared, cap)
                .flatMap(read -> ReactiveFormBodySupport.recoverConsumedBody(exchange, read, declared));
        return Mono.zip(body, principal).flatMap(parts -> {
            byte[] bodyBytes = parts.getT1();
            // A rebuilt form body can differ in length from the declared one.
            String contentLength = declared >= 0 && bodyBytes.length != declared
                    ? Integer.toString(bodyBytes.length) : null;
            VesperaBridge.HeaderSource headers = new ReactiveHeaderSource(request, appSelectionHeader,
                    requestIdHeaderLower, px.requestId, parts.getT2().orElse(null), contentLength);
            byte[] wire = VesperaBridge.encodeRequest(appName, method, px.path, query, headers, bodyBytes);
            px.reachedRust = true;
            return streaming ? stream(wire, out, px, method) : buffered(wire, out, px, method);
        });
    }

    private String resolveAppName(ServerWebExchange exchange, ProxyExchange px) {
        try {
            String app = appResolver.resolveAppName(exchange);
            px.app = app;
            return app;
        } catch (UnknownAppException unknown) {
            // Reported as AppTags.UNKNOWN; the body never echoes the name.
            px.app = unknown.appName();
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, VesperaProxyController.UNKNOWN_APP_MSG);
        }
    }

    /**
     * The request path relative to the application
     * ({@code spring.webflux.base-path} removed), raw (still percent-encoded);
     * the bare application root collapses to {@code "/"}.
     */
    static String pathWithinApplication(ServerHttpRequest request) {
        String path = request.getPath().pathWithinApplication().value();
        return path.isEmpty() ? "/" : path;
    }

    /**
     * Aggregate the request body without blocking: capped by {@code cap}
     * ({@code > 0}), else by the single-array bound — 64 MiB for an unknown
     * length (servlet parity). Exceeding the cap answers {@code 413}.
     */
    static Mono<byte[]> readBody(ServerHttpRequest request, long declared, long cap) {
        if (declared == 0) {
            return Mono.just(VesperaWireCodec.EMPTY_BODY);
        }
        long limit = cap > 0
                ? Math.min(cap, MAX_BUFFERED_BODY)
                : (declared > 0 ? MAX_BUFFERED_BODY : MAX_FIXED_BODY);
        return DataBufferUtils.join(request.getBody(), (int) limit)
                .map(buffer -> {
                    try {
                        byte[] bytes = new byte[buffer.readableByteCount()];
                        buffer.read(bytes);
                        return bytes;
                    } finally {
                        DataBufferUtils.release(buffer);
                    }
                })
                .defaultIfEmpty(VesperaWireCodec.EMPTY_BODY)
                .onErrorMap(DataBufferLimitException.class, tooLarge -> new ResponseStatusException(
                        HttpStatusCode.valueOf(413),
                        "buffered request body exceeds vespera.bridge.max-buffered-request-bytes="
                                + limit));
    }

    private static ResponseStatusException payloadTooLarge(long actualBytes, long capBytes) {
        // 413 as a raw code: CONTENT_TOO_LARGE / PAYLOAD_TOO_LARGE differ between Spring 6 and 7.
        return new ResponseStatusException(
                HttpStatusCode.valueOf(413),
                "buffered request body exceeds vespera.bridge.max-buffered-request-bytes="
                        + capBytes + " (actual " + actualBytes + " bytes)");
    }

    /**
     * Buffered dispatch over the non-blocking async symbol. Cancelling the
     * returned {@code Mono} (client disconnect) cancels the native future,
     * which aborts the Rust handler.
     */
    private Mono<Void> buffered(byte[] wire, ReactiveProxyResponse out, ProxyExchange px, String method) {
        CompletableFuture<byte[]> nativeResult = transport.dispatchAsync(wire);
        long cap = javaResponseCap(maxBufferedResponseBytes);
        return Mono.fromFuture(nativeResult)
                .doOnCancel(() -> nativeResult.cancel(false))
                // The future completes on a Rust Tokio worker: never run the
                // response build there.
                .publishOn(completionScheduler)
                .flatMap(wireResp -> ProxyExchange.withMdc(px.requestId,
                        () -> writeBuffered(wireResp, out, px, method, cap)));
    }

    /** The Java-side response cap still needed (0 when the native cap is at least as strict). */
    static long javaResponseCap(long cap) {
        return VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.enforcesAtMost(cap) ? 0 : cap;
    }

    static Mono<Void> writeBuffered(byte[] wire, ReactiveProxyResponse out, ProxyExchange px,
            String method, long cap) {
        int headerLen = VesperaWireCodec.readHeaderLength(wire);
        int bodyOff = 4 + headerLen;
        int bodyLen = wire.length - bodyOff;
        if (cap > 0 && bodyLen > cap) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "buffered response body exceeds vespera.bridge.max-buffered-response-bytes="
                            + cap + " (actual " + bodyLen + " bytes)");
        }
        HeaderPolicy.ResponseHeaderAccumulator acc = out.applyWireHeader(wire, 4, headerLen);
        out.setContentLength(WireResponseWriter.bufferedContentLength(
                acc.status, method, bodyLen, acc.declaredContentLength()));
        int toWrite = WireResponseWriter.responsePermitsBody(acc.status, method) ? bodyLen : 0;
        return out.writeBody(wire, bodyOff, toWrite)
                .doOnSuccess(ignored -> px.complete(acc.status, out.panicked()));
    }

    /**
     * Response streaming: the blocking native symbol runs on
     * {@link #streamingScheduler}; its {@code OutputStream} is bridged to the
     * response with backpressure (a write blocks until the client demands
     * more). The Rust header arrives before the first body byte, while the
     * response is still uncommitted.
     */
    private Mono<Void> stream(byte[] wire, ReactiveProxyResponse out, ProxyExchange px, String method) {
        ServerHttpResponse response = out.response;
        Executor executor = task -> streamingScheduler.schedule(task);
        Publisher<DataBuffer> body = DataBufferUtils.outputStreamPublisher(sink -> {
            BodyGate gate = new BodyGate(sink);
            try {
                transport.dispatchStreaming(wire, headerBytes -> ProxyExchange.withMdc(px.requestId, () -> {
                    gate.permit(applyStreamingHeader(headerBytes, out, method));
                    return null;
                }), gate);
            } catch (IOException failure) {
                throw new StreamAbort(failure);
            }
        }, response.bufferFactory(), executor, STREAM_CHUNK_BYTES);
        return response.writeAndFlushWith(Flux.from(body).map(Mono::just))
                .doOnSuccess(ignored -> px.complete(out.status(), out.panicked()));
    }

    /** Apply a streamed wire header; returns whether body bytes may follow. */
    static boolean applyStreamingHeader(byte[] headerBytes, ReactiveProxyResponse out, String method) {
        int headerLen = VesperaWireCodec.readHeaderLength(headerBytes);
        HeaderPolicy.ResponseHeaderAccumulator acc = out.applyWireHeader(headerBytes, 4, headerLen, true);
        return WireResponseWriter.responsePermitsBody(acc.status, method);
    }

    /**
     * The native streaming symbol failed after it started: normally a
     * {@link VesperaStreamAbortedException} (a panic or a body error after
     * the Rust header). Unchecked so it can leave the
     * {@code outputStreamPublisher} body; unwrapped again by {@link #fail}.
     */
    static final class StreamAbort extends RuntimeException {
        private static final long serialVersionUID = 1L;

        StreamAbort(IOException cause) {
            super(cause.getMessage(), cause, false, false);
        }

        @Override
        public synchronized IOException getCause() {
            return (IOException) super.getCause();
        }
    }

    /**
     * Handle a failed request. While the response is uncommitted it becomes a
     * {@link ProxyErrors}-shaped response ({@link ResponseStatusException} →
     * its status, a post-header stream abort → {@code 500}
     * {@link ProxyErrors#RUST_PANIC} / {@link ProxyErrors#STREAM_ABORTED},
     * anything else → {@code 500}), with an applied Rust header discarded
     * first (its status and headers never reach the client). Once committed
     * the error propagates, so the server aborts the connection: the client
     * sees a truncated transfer, never a cleanly completed short body. A
     * caught Rust panic is flagged for observers either way (servlet parity,
     * see {@link ProxyErrors#writeStreamAbort}).
     */
    private Mono<Void> fail(ReactiveProxyResponse out, ProxyExchange px, Throwable failure) {
        IOException aborted = failure instanceof StreamAbort streamAbort ? streamAbort.getCause() : null;
        boolean panic = aborted != null && VesperaStreamAbortedException.isPanic(aborted);
        if (panic) {
            out.markPanicked();
        }
        if (out.response.isCommitted()) {
            px.complete(out.status(), out.panicked());
            return Mono.error(aborted != null ? aborted : failure);
        }
        out.discardRustOutput();
        int status;
        String message;
        if (aborted != null) {
            status = 500;
            message = panic ? ProxyErrors.RUST_PANIC : ProxyErrors.STREAM_ABORTED;
        } else if (failure instanceof ResponseStatusException rejected) {
            status = rejected.getStatusCode().value();
            message = rejected.getReason();
        } else {
            ProxyExchange.withMdc(px.requestId, () -> {
                log.warn("vespera dispatch of {} {} failed requestId={}",
                        px.method, px.path, px.requestId, failure);
                return null;
            });
            status = 500;
            message = ProxyErrors.GENERIC_FAILURE;
        }
        return out.writeError(status, message)
                .doOnSuccess(ignored -> px.complete(status, out.panicked()));
    }

    private static ResponseStatusException notInitialized(UnsatisfiedLinkError unlinked) {
        log.warn("vespera native call failed: the native library is not loaded", unlinked);
        return new ResponseStatusException(HttpStatus.SERVICE_UNAVAILABLE, ProxyErrors.NOT_INITIALIZED);
    }

    /** Drops body writes once the Rust header forbids a body ({@code 204}, {@code 304}, ...). */
    static final class BodyGate extends OutputStream {
        private final OutputStream delegate;
        private volatile boolean permitsBody = true;

        BodyGate(OutputStream delegate) {
            this.delegate = delegate;
        }

        void permit(boolean permitsBody) {
            this.permitsBody = permitsBody;
        }

        @Override
        public void write(int b) throws IOException {
            if (permitsBody) {
                delegate.write(b);
            }
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (permitsBody && len > 0) {
                delegate.write(b, off, len);
            }
        }

        @Override
        public void flush() throws IOException {
            delegate.flush();
        }

        @Override
        public void close() throws IOException {
            delegate.close();
        }
    }
}
