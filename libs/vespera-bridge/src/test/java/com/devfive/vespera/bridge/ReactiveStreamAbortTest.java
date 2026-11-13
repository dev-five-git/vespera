package com.devfive.vespera.bridge;

import static com.devfive.vespera.bridge.ReactiveTestSupport.call;
import static com.devfive.vespera.bridge.ReactiveTestSupport.responseWire;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import com.devfive.vespera.bridge.ReactiveTestSupport.Result;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.context.annotation.AnnotationConfigApplicationContext;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.HttpHeaders;
import org.springframework.http.server.reactive.ReactorHttpHandlerAdapter;
import org.springframework.mock.http.server.reactive.MockServerHttpResponse;
import org.springframework.test.web.reactive.server.WebTestClient;
import org.springframework.web.reactive.config.EnableWebFlux;
import org.springframework.web.server.WebFilter;
import org.springframework.web.server.adapter.WebHttpHandlerBuilder;
import reactor.netty.DisposableServer;
import reactor.netty.http.server.HttpServer;

/**
 * The reactive twin of {@link StreamAbortHandlingTest}: a streamed response
 * that fails after the Rust header was applied. While the response is
 * uncommitted, Rust's status and headers are discarded and the proxy answers
 * its own {@code 500} (flagged panicked for a Rust panic); once committed, the
 * connection is aborted so the client never sees a cleanly completed body.
 */
class ReactiveStreamAbortTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    private static final String BODY_ABORTED =
            "vespera: response body stream aborted after the header was committed";

    /** A Rust header carrying headers that must not survive a proxy error. */
    private static final byte[] RUST_HEADER = responseWire(200,
            "{\"content-type\":\"text/event-stream\",\"set-cookie\":\"session=rust\","
                    + "\"cache-control\":\"max-age=600\",\"x-upstream\":\"rust\","
                    + "\"access-control-allow-origin\":\"https://rust\",\"x-request-id\":\"rust-rid\"}",
            "");

    private final FakeTransport transport = new FakeTransport();
    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();
    private final List<AnnotationConfigApplicationContext> contexts = new ArrayList<>();
    private DisposableServer server;

    @AfterEach
    void close() {
        if (server != null) {
            server.disposeNow();
        }
        contexts.forEach(AnnotationConfigApplicationContext::close);
    }

    private AnnotationConfigApplicationContext context() {
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1 << 20, 1 << 20,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST);
        AnnotationConfigApplicationContext ctx = new AnnotationConfigApplicationContext();
        ctx.register(WebFluxConfig.class);
        ctx.registerBean(VesperaReactiveHandlerMapping.class,
                () -> new VesperaReactiveHandlerMapping(handler, ProxyPathPatterns.defaults()));
        // A filter-owned header and a CORS header Rust's value replaces (CorsHandling.RUST).
        ctx.registerBean("securityHeaders", WebFilter.class, () -> (exchange, chain) -> {
            exchange.getResponse().getHeaders().set("X-Frame-Options", "DENY");
            exchange.getResponse().getHeaders().set("Access-Control-Allow-Origin", "https://spring");
            return chain.filter(exchange);
        });
        ctx.refresh();
        contexts.add(ctx);
        return ctx;
    }

    private Result stream() {
        return call(WebTestClient.bindToApplicationContext(context()).build().get().uri("/events")
                .header(HttpHeaders.ACCEPT, "text/event-stream")
                .header("X-Request-Id", "rid-1"));
    }

    private DispatchEvent awaitEvent() throws InterruptedException {
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (events.isEmpty() && System.nanoTime() < deadline) {
            Thread.sleep(5);
        }
        assertFalse(events.isEmpty(), "no DispatchEvent");
        return events.get(events.size() - 1);
    }

    @Test
    void uncommittedPanicIsReplacedByAPanicked500() throws Exception {
        transport.streamHeader = RUST_HEADER;
        transport.streamChunks = List.of();
        transport.streamFailure = new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);

        Result result = stream();

        assertEquals(500, result.status());
        assertEquals(ProxyErrors.CONTENT_TYPE, result.header(HttpHeaders.CONTENT_TYPE));
        assertEquals(ProxyErrors.RUST_PANIC, result.body());
        assertNull(result.header(HttpHeaders.SET_COOKIE), "Rust's Set-Cookie is dropped");
        assertNull(result.header(HttpHeaders.CACHE_CONTROL), "Rust's Cache-Control is dropped");
        assertNull(result.header("X-Upstream"));
        assertEquals("rid-1", result.header("X-Request-Id"), "the request-id echo is restored");
        assertEquals("DENY", result.header("X-Frame-Options"), "filter headers survive");
        assertEquals("https://spring", result.header("Access-Control-Allow-Origin"),
                "a filter value Rust replaced is restored");
        DispatchEvent event = awaitEvent();
        assertEquals(500, event.status());
        assertTrue(event.panicked());
        assertEquals(DispatchMode.STREAMING, event.mode());
    }

    @Test
    void uncommittedBodyAbortIsReplacedByA500() throws Exception {
        transport.streamHeader = RUST_HEADER;
        transport.streamChunks = List.of();
        transport.streamFailure = new VesperaStreamAbortedException(BODY_ABORTED);

        Result result = stream();

        assertEquals(500, result.status());
        assertEquals(ProxyErrors.STREAM_ABORTED, result.body());
        assertNull(result.header(HttpHeaders.SET_COOKIE));
        DispatchEvent event = awaitEvent();
        assertEquals(500, event.status());
        assertFalse(event.panicked());
    }

    @Test
    void plainIOExceptionWithThePanicMessageCountsAsAPanic() throws Exception {
        // Fallback when the cdylib could not resolve the typed exception class.
        transport.streamHeader = RUST_HEADER;
        transport.streamChunks = List.of();
        transport.streamFailure = new IOException(VesperaStreamAbortedException.PANIC_MESSAGE);

        Result result = stream();

        assertEquals(500, result.status());
        assertEquals(ProxyErrors.RUST_PANIC, result.body());
        assertTrue(awaitEvent().panicked());
    }

    @Test
    void committedPanicAbortsTheConnectionAndIsReportedAsAPanic() throws Exception {
        transport.streamChunks = List.of("data: 1\n\n");
        transport.streamFailure = new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);
        server = HttpServer.create().host("127.0.0.1").port(0)
                .handle(new ReactorHttpHandlerAdapter(WebHttpHandlerBuilder.applicationContext(context()).build()))
                .bindNow();
        HttpClient http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + server.port() + "/events"))
                .header("Accept", "text/event-stream").timeout(Duration.ofSeconds(10)).build();

        // The 200 header and the first event were sent: the transfer is cut
        // short instead of completing with a truncated body.
        assertThrows(IOException.class, () -> http.send(request, HttpResponse.BodyHandlers.ofString()));

        DispatchEvent event = awaitEvent();
        assertEquals(200, event.status());
        assertTrue(event.panicked());
    }

    @Test
    void discardWithoutASnapshotDropsTheRustHeadersByName() {
        MockServerHttpResponse response = new MockServerHttpResponse();
        response.getHeaders().set("X-Frame-Options", "DENY");
        ReactiveProxyResponse out = new ReactiveProxyResponse(response, "X-Request-Id", "rid-1", CorsHandling.RUST);
        out.applyDefaultRequestId();
        out.applyWireHeader(RUST_HEADER, 4, RUST_HEADER.length - 4);
        assertEquals("rust-rid", response.getHeaders().getFirst("X-Request-Id"));
        assertTrue(out.headerApplied());

        out.discardRustOutput();

        assertFalse(out.headerApplied());
        assertNull(response.getHeaders().getFirst(HttpHeaders.SET_COOKIE));
        assertNull(response.getHeaders().getFirst("X-Upstream"));
        assertEquals("DENY", response.getHeaders().getFirst("X-Frame-Options"));
        assertEquals("rid-1", response.getHeaders().getFirst("X-Request-Id"));
    }
}
