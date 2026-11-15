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
import java.util.concurrent.RejectedExecutionException;
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
 * ({@link VesperaBridge#dispatchStreamingWithHeader}) on a dedicated, bounded
 * {@link VesperaStreamingExecutor} (never Reactor's shared
 * {@code boundedElastic}), bridged with backpressure through
 * {@link DataBufferUtils#outputStreamPublisher}: each stream occupies one
 * streaming thread while it runs, and a client disconnect stops the Rust
 * stream at its next write. When every streaming thread is busy and the
 * streaming queue is full ({@code vespera.bridge.reactive-streaming.*}), the
 * request is answered {@code 503} before anything is committed.
 *
 * <p>A bodyless {@code GET} whose {@code Accept} could still match a stream
 * (none, {@code *}{@code /*}, {@code text/*}, {@code application/*}) takes the
 * same header-first path, since an event stream it did not ask for would
 * never complete on the buffered path. Its body is held in Java until it is
 * complete (written with its {@code Content-Length}, like a buffered
 * response) or turns out to be a stream: an event / NDJSON
 * {@code Content-Type}, a Rust body that has to wait for its next frame, or
 * more than {@code min(max-buffered-response-bytes, }{@value #IMPLICIT_BUFFER_BYTES}{@code )}
 * bytes ({@value #IMPLICIT_BUFFER_BYTES} when the cap is unlimited). When the streaming
 * executor is saturated such a request falls back to the buffered path
 * instead of {@code 503}. Like every streamed response (and the servlet
 * proxy's SYNC / DIRECT modes) it is not cancelled while the Rust handler is
 * still working on the header; a disconnect stops it at its next write. A
 * concrete non-streaming {@code Accept} ({@code application/json}) keeps the
 * cancellable buffered path.
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

    /** Body bytes an implicitly streamed GET holds back for a {@code Content-Length} before streaming. */
    static final int IMPLICIT_BUFFER_BYTES = 1024 * 1024;

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
    private final VesperaStreamingExecutor streamingExecutor;

    /** Handler with the defaults: no principal, 64 MiB buffered caps, no request ids. */
    public VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver) {
        this(appResolver, null, VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
                VesperaProxyController.DEFAULT_MAX_BUFFERED_RESPONSE_BYTES, null, List.of(), null);
    }

    /**
     * Constructor with a default {@link VesperaStreamingExecutor}
     * ({@link VesperaStreamingExecutor#defaultMaxThreads()} threads,
     * {@value VesperaStreamingExecutor#DEFAULT_MAX_QUEUED} queued) owned by
     * this handler; its idle threads time out on their own.
     *
     * @see #VesperaReactiveProxyHandler(ReactiveAppNameResolver, ReactivePrincipalResolver, long,
     *      long, String, List, CorsHandling, VesperaStreamingExecutor)
     */
    public VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver,
                                       ReactivePrincipalResolver principalResolver,
                                       long maxBufferedRequestBytes,
                                       long maxBufferedResponseBytes,
                                       String requestIdHeader,
                                       List<DispatchObserver> observers,
                                       CorsHandling cors) {
        this(appResolver, principalResolver, maxBufferedRequestBytes, maxBufferedResponseBytes,
                requestIdHeader, observers, cors,
                new VesperaStreamingExecutor(0, VesperaStreamingExecutor.DEFAULT_MAX_QUEUED));
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
     * @param streamingExecutor       where streamed responses run (the caller closes it)
     */
    public VesperaReactiveProxyHandler(ReactiveAppNameResolver appResolver,
                                       ReactivePrincipalResolver principalResolver,
                                       long maxBufferedRequestBytes,
                                       long maxBufferedResponseBytes,
                                       String requestIdHeader,
                                       List<DispatchObserver> observers,
                                       CorsHandling cors,
                                       VesperaStreamingExecutor streamingExecutor) {
        this(appResolver, principalResolver, maxBufferedRequestBytes, maxBufferedResponseBytes,
                requestIdHeader, observers, cors, NATIVE, Schedulers.parallel(), streamingExecutor);
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
                                VesperaStreamingExecutor streamingExecutor) {
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
        this.streamingExecutor = Objects.requireNonNull(streamingExecutor, "streamingExecutor");
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
        String previousMdcId = requestId == null ? null : MDC.get(VesperaProxyController.MDC_REQUEST_ID);
        if (requestId != null) {
            MDC.put(VesperaProxyController.MDC_REQUEST_ID, requestId);
        }
        try {
            flow = dispatch(exchange, out, px, method);
        } catch (RuntimeException failure) {
            flow = Mono.error(failure);
        } finally {
            if (requestId != null) {
                ProxyExchange.restoreMdc(previousMdcId);
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
        String accept = requestHeaders.getFirst(HttpHeaders.ACCEPT);
        final boolean streaming = !"HEAD".equals(method)
                && SmartDispatchModeResolver.acceptsStreaming(accept);
        // A bodyless GET that did not ask for a stream may still get one (an
        // SSE / NDJSON endpoint fetched with `Accept: */*`): buffered, it would
        // never be sent. It takes the header-first path too, buffering the body
        // in Java until it would wait (servlet DIRECT parity).
        final boolean implicitStreaming = !streaming
                && "GET".equals(method)
                && bodyless(requestHeaders)
                && SmartDispatchModeResolver.mayAcceptStreaming(accept);
        px.mode = streaming || implicitStreaming ? DispatchMode.STREAMING : DispatchMode.ASYNC;
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
        // form body through exchange.getFormData(): rebuild it from that. A
        // consumed multipart body cannot be rebuilt: 500, never forwarded empty.
        Mono<byte[]> body = readBody(request, declared, cap)
                .onErrorResume(IllegalStateException.class,
                        failure -> ReactiveFormBodySupport.secondMultipartRead(exchange, failure))
                .flatMap(read -> ReactiveFormBodySupport.recoverConsumedBody(exchange, read, declared))
                .map(read -> ReactiveFormBodySupport.rejectConsumedMultipart(exchange, read, declared));
        return Mono.zip(body, principal).flatMap(parts -> {
            byte[] bodyBytes = parts.getT1();
            // A rebuilt form body can differ in length from the declared one.
            String contentLength = declared >= 0 && bodyBytes.length != declared
                    ? Integer.toString(bodyBytes.length) : null;
            VesperaBridge.HeaderSource headers = new ReactiveHeaderSource(request, appSelectionHeader,
                    requestIdHeaderLower, px.requestId, parts.getT2().orElse(null), contentLength);
            byte[] wire = VesperaBridge.encodeRequest(appName, method, px.path, query, headers, bodyBytes);
            px.reachedRust = true;
            if (streaming) {
                return stream(wire, out, px, method, false);
            }
            return implicitStreaming ? stream(wire, out, px, method, true) : buffered(wire, out, px, method);
        });
    }

    /** No request body: {@code Content-Length} absent or {@code 0}, no {@code Transfer-Encoding}. */
    static boolean bodyless(HttpHeaders headers) {
        return headers.getContentLength() <= 0 && headers.getFirst(HttpHeaders.TRANSFER_ENCODING) == null;
    }

    /** Who owns CORS ({@code vespera.bridge.cors}), or {@code null} when unset. */
    CorsHandling cors() {
        return cors;
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
     * ({@code spring.webflux.base-path} removed), raw (still percent-encoded),
     * without {@code ;} path parameters ({@code ;jsessionid=…}, matrix
     * variables — servlet parity, {@link PathParameters}); the bare
     * application root collapses to {@code "/"}.
     */
    static String pathWithinApplication(ServerHttpRequest request) {
        String path = PathParameters.strip(request.getPath().pathWithinApplication().value());
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
                        HttpStatusCode.valueOf(413), bodyTooLargeMessage(cap, limit)));
    }

    /**
     * The {@code 413} reason: the configured cap, or — with
     * {@code max-buffered-request-bytes=0} (unlimited) — the built-in bound
     * that applied instead (servlet parity), never a value the user did not set.
     */
    static String bodyTooLargeMessage(long cap, long limit) {
        return cap > 0
                ? "buffered request body exceeds vespera.bridge.max-buffered-request-bytes=" + limit
                : "buffered request body exceeds the built-in " + limit
                        + "-byte bound (vespera.bridge.max-buffered-request-bytes=0)";
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
     * {@link #streamingExecutor}; its {@code OutputStream} is bridged to the
     * response with backpressure (a write blocks until the client demands
     * more). The Rust header arrives before the first body byte, while the
     * response is still uncommitted. A saturated executor answers {@code 503}
     * without dispatching, as does one that drops the queued task unrun
     * because no streaming thread could start.
     *
     * <p>{@code implicit} (a bodyless GET that did not ask for a stream): a
     * saturated executor falls back to the buffered {@link #buffered} path
     * instead of {@code 503}, and the body is held in Java
     * ({@link SpillingGate}) so an ordinary response keeps its
     * {@code Content-Length}; it starts streaming as soon as the Rust body
     * would wait (the native flush hint), grows past
     * {@code min(maxBufferedResponseBytes, }{@value #IMPLICIT_BUFFER_BYTES}{@code )} bytes; a Rust
     * event / NDJSON {@code Content-Type} gets the explicit stream's hold
     * (no {@code Content-Length}).
     */
    private Mono<Void> stream(byte[] wire, ReactiveProxyResponse out, ProxyExchange px, String method,
            boolean implicit) {
        VesperaStreamingExecutor.Reservation slot = streamingExecutor.tryReserve();
        if (slot == null) {
            return saturated(wire, out, px, method, implicit);
        }
        ServerHttpResponse response = out.response;
        Executor executor = slot.executor();
        long cap = maxBufferedResponseBytes > 0
                ? Math.min(maxBufferedResponseBytes, IMPLICIT_BUFFER_BYTES) : IMPLICIT_BUFFER_BYTES;
        Publisher<DataBuffer> body = DataBufferUtils.outputStreamPublisher(sink -> {
            BodyGate gate = implicit ? new SpillingGate(sink, out, method, (int) cap) : new BodyGate(sink);
            try {
                transport.dispatchStreaming(wire, headerBytes -> ProxyExchange.withMdc(px.requestId, () -> {
                    int headerLen = VesperaWireCodec.readHeaderLength(headerBytes);
                    gate.onHeader(out.applyWireHeader(headerBytes, 4, headerLen, true), method);
                    return null;
                }), gate);
                gate.finish();
            } catch (IOException failure) {
                throw new StreamAbort(failure);
            }
        }, response.bufferFactory(), executor, STREAM_CHUNK_BYTES);
        // The pool dropped the queued task unrun (no thread could start):
        // fail the body instead of waiting for a thread that may never come.
        Mono<DataBuffer> dropped = Mono.fromCompletionStage(slot.rejection()).flatMap(Mono::error);
        return response.writeAndFlushWith(Flux.from(body).mergeWith(dropped).map(Mono::just))
                .doOnSuccess(ignored -> px.complete(out.status(), out.panicked()))
                // Cancelled / failed before the pool ran the task (never
                // submitted, or still queued): free the slot and drop the
                // task, so a disconnected client never reaches Rust.
                .doFinally(signal -> slot.cancel())
                // Refused by the pool before Rust ran (closed, or no thread
                // could start; the slot is already free): saturation handling.
                .onErrorResume(failure -> failure instanceof RejectedExecutionException && slot.wasRejected(),
                        failure -> saturated(wire, out, px, method, implicit));
    }

    /** No streaming thread for this request: implicit streams buffer, explicit ones answer {@code 503}. */
    private Mono<Void> saturated(byte[] wire, ReactiveProxyResponse out, ProxyExchange px, String method,
            boolean implicit) {
        if (implicit) {
            px.mode = DispatchMode.ASYNC;
            return buffered(wire, out, px, method);
        }
        px.reachedRust = false;
        return Mono.error(new ResponseStatusException(HttpStatus.SERVICE_UNAVAILABLE,
                ProxyErrors.STREAMING_SATURATED));
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

    /**
     * The explicit-stream body: drops body writes once the Rust header forbids
     * a body ({@code 204}, {@code 304}, ...) and holds the first body bytes
     * back until the native flush hint (the Rust body would wait), more than
     * {@value #STREAM_CHUNK_BYTES} bytes, or the end of the body. Without the
     * hold, a body that fails before its first flush hint would still reach
     * the client: {@code outputStreamPublisher} flushes its own buffer when
     * the failed handler leaves it, which commits Rust's status and headers
     * before {@link #fail} can answer the proxy {@code 500}. Used by one
     * streaming thread at a time.
     */
    static class BodyGate extends OutputStream {
        final OutputStream delegate;
        volatile boolean permitsBody = true;
        /** Whether body bytes are still held back ({@code false} = pass-through). */
        private boolean holding = true;
        /** Held body bytes, allocated on the first held write. */
        private java.io.ByteArrayOutputStream pending;

        BodyGate(OutputStream delegate) {
            this.delegate = delegate;
        }

        void permit(boolean permitsBody) {
            this.permitsBody = permitsBody;
        }

        /** The Rust header was applied to the response (before any body byte). */
        void onHeader(HeaderPolicy.ResponseHeaderAccumulator acc, String method) {
            permit(WireResponseWriter.responsePermitsBody(acc.status, method));
        }

        /** Whether body bytes are still held back (test hook). */
        boolean pending() {
            return holding;
        }

        private void release() throws IOException {
            java.io.ByteArrayOutputStream bytes = pending;
            pending = null;
            holding = false;
            writeHeld(bytes, delegate);
        }

        /** The native dispatch returned normally: the body is complete. */
        void finish() throws IOException {
            release();
        }

        @Override
        public void write(int b) throws IOException {
            write(new byte[] {(byte) b}, 0, 1);
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (!permitsBody || len <= 0) {
                return;
            }
            if (!holding) {
                delegate.write(b, off, len);
                return;
            }
            pending = hold(pending, b, off, len, STREAM_CHUNK_BYTES);
            if (pending == null) {
                // Past the limit: release what was held, then this write.
                holding = false;
                delegate.write(b, off, len);
            }
        }

        /**
         * Append {@code b[off, off+len)} to {@code held} (allocated lazily,
         * sized to the first write), or return {@code null} — after writing
         * the earlier held bytes to {@link #delegate}, with this write NOT
         * appended — when the held total would exceed {@code limit}: the
         * caller then writes {@code b} straight through (no copy of a large
         * first write into the hold buffer).
         */
        final java.io.ByteArrayOutputStream hold(java.io.ByteArrayOutputStream held,
                byte[] b, int off, int len, int limit) throws IOException {
            int size = held == null ? 0 : held.size();
            if ((long) size + len > limit) {
                writeHeld(held, delegate);
                return null;
            }
            if (held == null) {
                held = new java.io.ByteArrayOutputStream(Math.max(len, 256));
            }
            held.write(b, off, len);
            return held;
        }

        /** Write held bytes without the {@code toByteArray()} copy. */
        static void writeHeld(java.io.ByteArrayOutputStream held, OutputStream to) throws IOException {
            if (held != null && held.size() > 0) {
                held.writeTo(to);
            }
        }

        @Override
        public void flush() throws IOException {
            release();
            delegate.flush();
        }

        @Override
        public void close() throws IOException {
            delegate.close();
        }
    }

    /**
     * The implicit-streaming body ({@link #stream} with {@code implicit}):
     * holds the body back so a complete in-memory response is written once
     * with its {@code Content-Length}, like the buffered path, and switches
     * to pass-through ("spills") as soon as the response is a stream: the
     * native flush hint (the Rust body is about to wait — never sent for an
     * in-memory body) or more than {@code limit} bytes. An event / NDJSON
     * {@code Content-Type} gets the explicit stream's {@link BodyGate} hold
     * instead: held up to {@value #STREAM_CHUNK_BYTES} bytes, released on the
     * flush hint, past that limit or at the end of the body, never with a
     * {@code Content-Length} — so a stream that fails before its first flush
     * hint is still answered with the proxy {@code 500}, whatever the client's
     * {@code Accept}. Used by one streaming thread at a time.
     */
    static final class SpillingGate extends BodyGate {
        private final ReactiveProxyResponse out;
        private final String method;
        /** Hold limit: {@code limit}, or {@value #STREAM_CHUNK_BYTES} for a stream {@code Content-Type}. */
        private int limit;
        /** Whether the Rust {@code Content-Type} is an event / NDJSON stream (no {@code Content-Length}). */
        private boolean streamType;
        private HeaderPolicy.ResponseHeaderAccumulator acc;
        /** Whether the body is still held back ({@code false} once spilled). */
        private boolean holding = true;
        /** Held body bytes, allocated on the first held write. */
        private java.io.ByteArrayOutputStream held;

        SpillingGate(OutputStream delegate, ReactiveProxyResponse out, String method, int limit) {
            super(delegate);
            this.out = out;
            this.method = method;
            this.limit = Math.max(0, limit);
        }

        @Override
        void onHeader(HeaderPolicy.ResponseHeaderAccumulator acc, String method) {
            super.onHeader(acc, method);
            this.acc = acc;
            for (HeaderPolicy.HeaderPair header : acc.headers) {
                if (header.name().equalsIgnoreCase(HttpHeaders.CONTENT_TYPE)
                        && SmartDispatchModeResolver.isStreamingMediaType(header.value())) {
                    // A stream: hold like the explicit BodyGate (nothing is held yet).
                    streamType = true;
                    limit = STREAM_CHUNK_BYTES;
                    return;
                }
            }
        }

        /** Whether the body is still held back (test hook). */
        boolean holding() {
            return holding;
        }

        /** The base gate's hold is unused here: report this gate's own state. */
        @Override
        boolean pending() {
            return holding;
        }

        private void spill() throws IOException {
            java.io.ByteArrayOutputStream bytes = held;
            held = null;
            holding = false;
            writeHeld(bytes, delegate);
        }

        @Override
        public void write(int b) throws IOException {
            write(new byte[] {(byte) b}, 0, 1);
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (!permitsBody || len <= 0) {
                return;
            }
            if (!holding) {
                delegate.write(b, off, len);
                return;
            }
            held = hold(held, b, off, len, limit);
            if (held == null) {
                // Past the limit: spilled what was held, now this write.
                holding = false;
                delegate.write(b, off, len);
            }
        }

        @Override
        public void flush() throws IOException {
            // The native flush hint: the Rust body would wait — a stream.
            spill();
            delegate.flush();
        }

        @Override
        void finish() throws IOException {
            if (!holding) {
                return;
            }
            java.io.ByteArrayOutputStream bytes = held;
            held = null;
            holding = false;
            int size = bytes == null ? 0 : bytes.size();
            if (acc != null && !streamType) {
                out.setContentLength(WireResponseWriter.bufferedContentLength(
                        acc.status, method, size, acc.declaredContentLength()));
            }
            writeHeld(bytes, delegate);
        }
    }
}
