package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ch.qos.logback.classic.Level;
import ch.qos.logback.classic.Logger;
import ch.qos.logback.classic.spi.ILoggingEvent;
import ch.qos.logback.core.read.ListAppender;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.reflect.Field;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.function.Consumer;
import org.junit.jupiter.api.Test;
import org.reactivestreams.Publisher;
import org.slf4j.LoggerFactory;
import org.springframework.core.io.buffer.DataBuffer;
import org.springframework.http.HttpHeaders;
import org.springframework.http.ResponseEntity;
import org.springframework.http.server.reactive.ServerHttpResponse;
import org.springframework.http.server.reactive.ServerHttpResponseDecorator;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.http.server.reactive.MockServerHttpResponse;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.web.context.request.async.DeferredResult;
import org.springframework.web.server.ResponseStatusException;
import org.springframework.web.server.ServerWebExchange;
import org.springframework.web.server.ServerWebExchangeDecorator;
import reactor.core.publisher.Mono;
import reactor.core.scheduler.Schedulers;

/**
 * Every failure path of the proxy logs exactly one WARN for the request, and
 * that line carries the cause (the throwable of the WARN event). The sync,
 * native-unlinked, async and WebFlux paths each used to log a second,
 * cause-less line from the failure handler.
 */
class ProxyFailureLogTest {

    private static Logger logger(Class<?> type) {
        return (Logger) LoggerFactory.getLogger(type);
    }

    private static ListAppender<ILoggingEvent> capture(Class<?> type) {
        ListAppender<ILoggingEvent> appender = new ListAppender<>();
        appender.start();
        logger(type).addAppender(appender);
        return appender;
    }

    private static void release(Class<?> type, ListAppender<ILoggingEvent> appender) {
        logger(type).detachAppender(appender);
    }

    private static List<ILoggingEvent> warns(ListAppender<ILoggingEvent> appender) {
        return appender.list.stream().filter(event -> event.getLevel() == Level.WARN).toList();
    }

    /** Set a private static flag of {@link VesperaBridge}; the caller restores it. */
    private static void setBridgeField(String name, Object value) throws Exception {
        Field field = VesperaBridge.class.getDeclaredField(name);
        field.setAccessible(true);
        field.set(null, value);
    }

    private static Object bridgeField(String name) throws Exception {
        Field field = VesperaBridge.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(null);
    }

    @Test
    void syncRuntimeFailureIsAnsweredWithOneWarnCarryingTheCause() throws Exception {
        ListAppender<ILoggingEvent> appender = capture(VesperaProxyController.class);
        try {
            VesperaProxyController controller = new VesperaProxyController(
                    request -> {
                        throw new IllegalStateException("boom");
                    },
                    request -> DispatchMode.SYNC, Runnable::run, true, 1024, 1024, null, List.of());
            MockHttpServletResponse response = new MockHttpServletResponse();

            controller.proxy(new MockHttpServletRequest("GET", "/x"), response);

            assertEquals(500, response.getStatus());
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the failure is logged once, not twice");
            assertTrue(warns.get(0).getFormattedMessage().startsWith("vespera server error on GET /x"),
                    warns.get(0).getFormattedMessage());
            assertEquals("boom", warns.get(0).getThrowableProxy().getMessage());
        } finally {
            release(VesperaProxyController.class, appender);
        }
    }

    @Test
    void unlinkedNativeIsAnsweredWith503AndOneWarnCarryingTheError() throws Exception {
        Object previousLoaded = bridgeField("loaded");
        Object previousNative = bridgeField("nativeLoaded");
        ListAppender<ILoggingEvent> appender = capture(VesperaProxyController.class);
        try {
            // Initialised as far as the proxy can tell, but the natives are not linked:
            // the first native call throws UnsatisfiedLinkError.
            setBridgeField("nativeLoaded", true);
            setBridgeField("loaded", true);
            VesperaProxyController controller = new VesperaProxyController(
                    request -> AppNameResolver.DEFAULT_APP, request -> DispatchMode.SYNC,
                    Runnable::run, true, 1024, 1024, null, List.of());
            MockHttpServletResponse response = new MockHttpServletResponse();

            controller.proxy(new MockHttpServletRequest("GET", "/y"), response);

            assertEquals(503, response.getStatus());
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the unlinked native is logged once, not twice");
            assertEquals("java.lang.UnsatisfiedLinkError",
                    warns.get(0).getThrowableProxy().getCause().getClassName());
        } finally {
            setBridgeField("loaded", previousLoaded);
            setBridgeField("nativeLoaded", previousNative);
            release(VesperaProxyController.class, appender);
        }
    }

    @Test
    void asyncFailureIsSettledWithOneWarnCarryingTheCause() {
        ListAppender<ILoggingEvent> appender = capture(VesperaProxyController.class);
        try {
            ProxyServletResponse proxied = new ProxyServletResponse(
                    new MockHttpServletResponse(), "X-Request-Id", "rid-9");
            DeferredResult<ResponseEntity<?>> result = AsyncProxyResponses.newAsyncResult(proxied);
            ProxyExchange exchange = new ProxyExchange(
                    LoggerFactory.getLogger(VesperaProxyController.class),
                    List.of(), "GET", "rid-9");
            exchange.path = "/async";

            // The native result failed on the response executor: the stage carries the
            // failure wrapped in a CompletionException, as whenComplete receives it.
            AsyncProxyResponses.settle(result, exchange, proxied, null,
                    new CompletionException(new IllegalStateException("boom")));

            assertTrue(result.hasResult());
            assertEquals(500, ((ResponseEntity<?>) result.getResult()).getStatusCode().value());
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the failure is logged once, not twice");
            assertTrue(warns.get(0).getFormattedMessage().startsWith("vespera server error on GET /async"),
                    warns.get(0).getFormattedMessage());
            assertEquals("boom", warns.get(0).getThrowableProxy().getMessage());
        } finally {
            release(VesperaProxyController.class, appender);
        }
    }

    @Test
    void webFluxDispatchFailureIsAnsweredWithOneWarnCarryingTheCause() {
        ListAppender<ILoggingEvent> appender = capture(VesperaReactiveProxyHandler.class);
        try {
            VesperaReactiveProxyHandler.Transport failing = new VesperaReactiveProxyHandler.Transport() {
                @Override
                public boolean isInitialized() {
                    return true;
                }

                @Override
                public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
                    throw new IllegalStateException("boom");
                }

                @Override
                public void dispatchStreaming(byte[] wireRequest, Consumer<byte[]> headerConsumer,
                        OutputStream body) {
                    throw new IllegalStateException("unused");
                }
            };
            VesperaReactiveProxyHandler handler = new VesperaReactiveProxyHandler(
                    new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
                    "X-Request-Id", List.of(), CorsHandling.RUST, failing, Schedulers.parallel(),
                    new VesperaStreamingExecutor(1, 1));
            MockServerWebExchange exchange = MockServerWebExchange.from(
                    MockServerHttpRequest.get("/orders").header(HttpHeaders.ACCEPT, "application/json"));

            handler.handle(exchange).block(Duration.ofSeconds(5));

            assertEquals(500, exchange.getResponse().getStatusCode().value());
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the failure is logged once, not twice");
            assertEquals("boom", warns.get(0).getThrowableProxy().getMessage());
        } finally {
            release(VesperaReactiveProxyHandler.class, appender);
        }
    }

    @Test
    void webFluxErrorAnswerWhoseWriteFailsIsStillReportedWithItsCause() {
        ListAppender<ILoggingEvent> appender = capture(VesperaReactiveProxyHandler.class);
        try {
            VesperaReactiveProxyHandler.Transport failing = new VesperaReactiveProxyHandler.Transport() {
                @Override
                public boolean isInitialized() {
                    return true;
                }

                @Override
                public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
                    throw new IllegalStateException("boom");
                }

                @Override
                public void dispatchStreaming(byte[] wireRequest, Consumer<byte[]> headerConsumer,
                        OutputStream body) {
                    throw new IllegalStateException("unused");
                }
            };
            VesperaReactiveProxyHandler handler = new VesperaReactiveProxyHandler(
                    new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
                    "X-Request-Id", List.of(), CorsHandling.RUST, failing, Schedulers.parallel(),
                    new VesperaStreamingExecutor(1, 1));
            // The client is gone: writing the proxy's error body fails.
            ServerHttpResponse goneClient = new ServerHttpResponseDecorator(new MockServerHttpResponse()) {
                @Override
                public Mono<Void> writeWith(Publisher<? extends DataBuffer> body) {
                    return Mono.error(new IOException("client gone"));
                }
            };
            ServerWebExchange exchange = new ServerWebExchangeDecorator(MockServerWebExchange.from(
                    MockServerHttpRequest.get("/orders").header(HttpHeaders.ACCEPT, "application/json"))) {
                @Override
                public ServerHttpResponse getResponse() {
                    return goneClient;
                }
            };

            assertThrows(RuntimeException.class, () -> handler.handle(exchange).block(Duration.ofSeconds(5)),
                    "the failed write is the error of the response");
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the failure is logged once, even though the write failed");
            assertEquals("boom", warns.get(0).getThrowableProxy().getMessage());
        } finally {
            release(VesperaReactiveProxyHandler.class, appender);
        }
    }

    @Test
    void bidirectionalStreamAbortAfterTheHeaderIsAnsweredWithOneWarnCarryingTheCause() throws Exception {
        ListAppender<ILoggingEvent> appender = capture(VesperaProxyController.class);
        try {
            ProxyServletResponse proxied = new ProxyServletResponse(
                    new MockHttpServletResponse(), "X-Request-Id", "rid-7");
            ProxyExchange exchange = new ProxyExchange(
                    LoggerFactory.getLogger(VesperaProxyController.class), List.of(), "POST", "rid-7");
            exchange.path = "/upload";
            // The native stream failed after its header was applied, so the response is still
            // uncommitted: the proxy answers 500 and the exchange reports the failure as its cause.
            DirectModeDispatcher.HeaderFirstStream stream = () -> {
                throw new IOException("body broke");
            };

            VesperaProxyController.streamBidirectional(stream, proxied,
                    new GatedRequestInputStream(new ByteArrayInputStream(new byte[0])), exchange);

            assertEquals(500, proxied.getStatus());
            List<ILoggingEvent> warns = warns(appender);
            assertEquals(1, warns.size(), "the abort is logged once, not twice");
            assertEquals("body broke", warns.get(0).getThrowableProxy().getMessage());
        } finally {
            release(VesperaProxyController.class, appender);
        }
    }

    @Test
    void knownLengthBodyAboveTheFrameBoundIsRefusedBeforeTheRead() throws Exception {
        // The frame bound is a parameter (production: VesperaWireCodec.MAX_FRAME_BODY). A
        // declared length above it must be refused as readBody refuses one above the
        // built-in bound, not read whole.
        ResponseStatusException refused = assertThrows(ResponseStatusException.class,
                () -> VesperaProxyController.readBufferedBody(
                        new ByteArrayInputStream(new byte[32]), 32, 100, 16));

        assertEquals(413, refused.getStatusCode().value());
        assertTrue(refused.getReason().contains("built-in 16-byte single-array bound"),
                refused.getReason());
        assertTrue(refused.getReason().contains("actual 32 bytes"), refused.getReason());
        assertEquals(16, VesperaProxyController.readBufferedBody(
                new ByteArrayInputStream(new byte[16]), 16, 100, 16).length);
    }
}
