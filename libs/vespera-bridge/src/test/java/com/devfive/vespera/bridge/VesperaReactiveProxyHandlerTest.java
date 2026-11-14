package com.devfive.vespera.bridge;

import static com.devfive.vespera.bridge.ReactiveTestSupport.call;
import static com.devfive.vespera.bridge.ReactiveTestSupport.responseWire;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import com.devfive.vespera.bridge.ReactiveTestSupport.Result;
import com.fasterxml.jackson.databind.JsonNode;
import java.net.URI;
import java.security.Principal;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.Consumer;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.context.annotation.AnnotationConfigApplicationContext;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.HttpHeaders;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.test.web.reactive.server.WebTestClient;
import org.springframework.web.bind.annotation.CrossOrigin;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.cors.CorsConfiguration;
import org.springframework.web.cors.reactive.CorsWebFilter;
import org.springframework.web.cors.reactive.UrlBasedCorsConfigurationSource;
import org.springframework.web.reactive.config.EnableWebFlux;
import org.springframework.web.server.WebFilter;
import reactor.core.Disposable;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;

/**
 * {@link VesperaReactiveProxyHandler} behaviour through a real WebFlux
 * {@code DispatcherHandler} ({@code WebTestClient} bound to an application
 * context with {@link VesperaReactiveHandlerMapping} /
 * {@link VesperaReactivePreflightFilter}), against a fake native transport.
 * Results are read through {@link ReactiveTestSupport#call} (not the
 * WebTestClient assertion DSL) so the suite also runs under {@code testBoot3}.
 */
class VesperaReactiveProxyHandlerTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    @RestController
    static class LocalController {
        @GetMapping("/local")
        String local() {
            return "spring";
        }

        @CrossOrigin(origins = "https://java.example")
        @GetMapping("/java/x")
        String javaCors() {
            return "java";
        }
    }

    private final FakeTransport transport = new FakeTransport();
    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();
    private final List<AnnotationConfigApplicationContext> contexts = new ArrayList<>();

    @AfterEach
    void closeContexts() {
        contexts.forEach(AnnotationConfigApplicationContext::close);
    }

    private VesperaReactiveProxyHandler handler(ReactiveAppNameResolver apps,
            ReactivePrincipalResolver principals, long maxRequest, long maxResponse, CorsHandling cors) {
        return ReactiveTestSupport.handler(transport, apps, principals, maxRequest, maxResponse,
                "X-Request-Id", List.of(events::add), cors);
    }

    private VesperaReactiveProxyHandler handler() {
        return handler(new ReactiveHeaderAppNameResolver("X-Vespera-App", List.of("admin")),
                null, 1024, 1024, CorsHandling.RUST);
    }

    private WebTestClient client(VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns,
            boolean preflightFilter, Consumer<AnnotationConfigApplicationContext> extra) {
        AnnotationConfigApplicationContext ctx = new AnnotationConfigApplicationContext();
        ctx.register(WebFluxConfig.class, LocalController.class);
        ctx.registerBean(VesperaReactiveHandlerMapping.class,
                () -> new VesperaReactiveHandlerMapping(handler, patterns));
        if (preflightFilter) {
            ctx.registerBean(VesperaReactivePreflightFilter.class,
                    () -> new VesperaReactivePreflightFilter(handler, patterns));
        }
        extra.accept(ctx);
        ctx.refresh();
        contexts.add(ctx);
        return WebTestClient.bindToApplicationContext(ctx).build();
    }

    private WebTestClient client(VesperaReactiveProxyHandler handler) {
        return client(handler, ProxyPathPatterns.defaults(), true, ctx -> { });
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
    void forwardsTheRequestAndWritesTheBufferedResponse() throws Exception {
        transport.response = responseWire(201,
                "{\"content-type\":\"application/json\",\"x-rust\":\"1\",\"transfer-encoding\":\"chunked\","
                        + "\"content-length\":\"999\"}",
                "{\"id\":42}");
        Result result = call(client(handler()).post()
                .uri(URI.create("http://localhost/users/42?expand=a%20b&x=1"))
                .header("X-Vespera-App", "admin")
                .header("X-Request-Id", "client-id")
                .header("Connection", "x-hop")
                .header("X-Hop", "dropped")
                .header("Keep-Alive", "timeout=5")
                .header("X-Multi", "a", "b")
                .header("Cookie", "a=1", "b=2")
                .header(HttpHeaders.CONTENT_TYPE, "application/json")
                .bodyValue("{\"name\":\"n\"}"));
        assertEquals(201, result.status());
        assertEquals("1", result.header("x-rust"));
        assertEquals("client-id", result.header("X-Request-Id"));
        assertEquals(9, result.headers().getContentLength(), "proxy-owned content-length");
        assertNull(result.header("transfer-encoding"), "hop-by-hop response header dropped");
        assertEquals("{\"id\":42}", result.body());

        JsonNode wire = transport.lastHeader();
        assertEquals("POST", wire.get("method").asText());
        // The wire header carries the raw query appended to the path.
        assertEquals("/users/42?expand=a%20b&x=1", wire.get("path").asText());
        assertEquals("admin", wire.get("app").asText());
        assertEquals("http", wire.get("scheme").asText());
        JsonNode headers = wire.get("headers");
        assertEquals("client-id", headers.get("x-request-id").asText());
        assertEquals("a, b", headers.get("x-multi").asText());
        assertEquals("a=1; b=2", headers.get("cookie").asText());
        assertEquals("application/json", headers.get("content-type").asText());
        assertNull(headers.get("x-vespera-app"), "selection header stripped");
        assertNull(headers.get("connection"));
        assertNull(headers.get("x-hop"), "Connection-nominated header dropped");
        assertNull(headers.get("keep-alive"));
        assertNull(wire.get("principal"));
        assertEquals("{\"name\":\"n\"}", transport.lastBody());

        DispatchEvent event = awaitEvent();
        assertEquals(201, event.status());
        assertEquals("admin", event.app());
        assertEquals(DispatchMode.ASYNC, event.mode());
        assertEquals("/users/42", event.path());
        assertEquals("client-id", event.requestId());
        assertFalse(event.panicked());
    }

    @Test
    void generatesARequestIdAndLetsRustOverrideTheEcho() {
        WebTestClient client = client(handler());
        Result generated = call(client.get().uri("/a"));
        assertEquals(200, generated.status());
        String id = generated.header("X-Request-Id");
        assertEquals(36, id.length());
        assertEquals(id, transport.lastHeader().get("headers").get("x-request-id").asText());

        transport.response = responseWire(200, "{\"x-request-id\":\"from-rust\"}", "");
        Result overridden = call(client.get().uri("/a").header("X-Request-Id", "bad id with spaces"));
        assertEquals(List.of("from-rust"), overridden.headers().getValuesAsList("X-Request-Id"));
    }

    @Test
    void unknownAppIs404WithoutDispatch() {
        Result result = call(client(handler()).get().uri("/users").header("X-Vespera-App", "secret"));
        assertEquals(404, result.status());
        assertEquals(ProxyErrors.CONTENT_TYPE, result.header(HttpHeaders.CONTENT_TYPE));
        assertEquals(VesperaProxyController.UNKNOWN_APP_MSG, result.body());
        assertTrue(transport.requests.isEmpty());
    }

    @Test
    void uninitializedNativeLibraryIs503() {
        transport.initialized = false;
        Result result = call(client(handler()).get().uri("/users"));
        assertEquals(503, result.status());
        assertEquals(ProxyErrors.NOT_INITIALIZED, result.body());
        assertTrue(transport.requests.isEmpty());
    }

    @Test
    void requestBodyOverTheCapIs413() throws Exception {
        WebTestClient client = client(handler());
        // Declared Content-Length over the cap: rejected before reading.
        assertEquals(413, call(client.post().uri("/upload").bodyValue("x".repeat(2000))).status());
        // Unknown length (streamed body): rejected while aggregating.
        Result streamed = call(client.post().uri("/upload")
                .body(Flux.range(0, 40).map(i -> "y".repeat(100)), String.class));
        assertEquals(413, streamed.status());
        assertTrue(streamed.body().contains("max-buffered-request-bytes=1024"), streamed.body());
        assertTrue(transport.requests.isEmpty());
        assertEquals(413, awaitEvent().status());
        // At the cap: dispatched.
        assertEquals(200, call(client.post().uri("/upload").bodyValue("z".repeat(1024))).status());
        assertEquals(1024, transport.lastBody().length());
    }

    @Test
    void responseBodyOverTheCapIs500() {
        transport.response = responseWire(200, "{}", "r".repeat(1025));
        Result result = call(client(handler()).get().uri("/big"));
        assertEquals(500, result.status());
        assertTrue(result.body().contains("max-buffered-response-bytes=1024"), result.body());
    }

    @Test
    void headAdvertisesTheDeclaredLengthWithoutABody() {
        transport.response = responseWire(200, "{\"content-length\":\"42\"}", "");
        Result result = call(client(handler()).head().uri("/doc"));
        assertEquals(200, result.status());
        assertEquals(42, result.headers().getContentLength());
        assertEquals("", result.body());
        assertEquals("HEAD", transport.lastHeader().get("method").asText());
    }

    @Test
    void noContentStatusDropsTheBody() {
        transport.response = responseWire(204, "{}", "ignored");
        Result result = call(client(handler()).delete().uri("/users/1"));
        assertEquals(204, result.status());
        assertEquals("", result.body());
    }

    @Test
    void panicIsReportedToObservers() throws Exception {
        transport.response = responseWire(500, "{}", "{\"version\":\"0.1.0\",\"panicked\":\"true\"}", "boom");
        assertEquals(500, call(client(handler()).get().uri("/panic")).status());
        DispatchEvent event = awaitEvent();
        assertTrue(event.panicked());
        assertEquals(500, event.status());
    }

    @Test
    void pathPatternsNarrowTheProxiedSpaceAndControllersWin() {
        WebTestClient client = client(handler(),
                new ProxyPathPatterns(List.of("/api/**", "/local"), List.of("/api/internal/**")),
                true, ctx -> { });
        assertEquals(200, call(client.get().uri("/api/users")).status());
        assertEquals(1, transport.requests.size());
        assertEquals(404, call(client.get().uri("/api/internal/x")).status());
        assertEquals(404, call(client.get().uri("/elsewhere")).status());
        // A WebFlux controller beats the catch-all even inside the proxied space.
        assertEquals("spring", call(client.get().uri("/local")).body());
        assertEquals(1, transport.requests.size());
    }

    @Test
    void rustAnswersCorsPreflightsByDefault() {
        transport.response = responseWire(204,
                "{\"access-control-allow-origin\":\"https://a.example\","
                        + "\"access-control-allow-methods\":\"PUT\"}", "");
        Result result = call(client(handler()).options().uri("/users")
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertEquals(204, result.status());
        assertEquals("https://a.example", result.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        assertEquals("OPTIONS", transport.lastHeader().get("method").asText());
    }

    @Test
    void preflightForAJavaEndpointIsAnsweredBySpring() {
        WebTestClient client = client(handler());
        Result allowed = call(client.options().uri(URI.create("http://localhost/java/x"))
                .header(HttpHeaders.ORIGIN, "https://java.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "GET"));
        assertEquals(200, allowed.status());
        assertEquals("https://java.example", allowed.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        Result foreign = call(client.options().uri(URI.create("http://localhost/java/x"))
                .header(HttpHeaders.ORIGIN, "https://evil.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "GET"));
        assertEquals(403, foreign.status(), "Spring applies the @CrossOrigin policy");
        assertTrue(transport.requests.isEmpty(), "never reached Rust");

        // The Rust space next to it keeps the Rust preflight.
        call(client.options().uri("/users")
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertEquals(1, transport.requests.size());
        assertEquals("OPTIONS", transport.lastHeader().get("method").asText());
    }

    @Test
    void withoutThePreflightFilterSpringKeepsPreflights() {
        call(client(handler(), ProxyPathPatterns.defaults(), false, ctx -> { }).options().uri("/users")
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertTrue(transport.requests.isEmpty(), "DispatcherHandler never runs a handler for a preflight");
    }

    private Consumer<AnnotationConfigApplicationContext> springCors() {
        return ctx -> ctx.registerBean(CorsWebFilter.class, () -> {
            CorsConfiguration config = new CorsConfiguration();
            config.addAllowedOrigin("https://spring.example");
            config.addAllowedMethod("*");
            UrlBasedCorsConfigurationSource source = new UrlBasedCorsConfigurationSource();
            source.registerCorsConfiguration("/**", config);
            return new CorsWebFilter(source);
        });
    }

    @Test
    void corsHeadersAreDeduplicatedPerOwner() {
        transport.response = responseWire(200, "{\"access-control-allow-origin\":\"*\"}", "ok");
        // An absolute URI: CorsWebFilter rejects a cross-origin check against a host-less request.
        URI cross = URI.create("http://localhost/c");
        VesperaReactiveProxyHandler springOwned = handler(
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1024, 1024, CorsHandling.SPRING);
        Result spring = call(client(springOwned, ProxyPathPatterns.defaults(), false, springCors())
                .get().uri(cross).header(HttpHeaders.ORIGIN, "https://spring.example"));
        assertEquals(List.of("https://spring.example"),
                spring.headers().getValuesAsList(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));

        VesperaReactiveProxyHandler rustOwned = handler(
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1024, 1024, CorsHandling.RUST);
        Result rust = call(client(rustOwned, ProxyPathPatterns.defaults(), true, springCors())
                .get().uri(cross).header(HttpHeaders.ORIGIN, "https://spring.example"));
        assertEquals(List.of("*"), rust.headers().getValuesAsList(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    /** Fake Spring Security types (Spring Security is not on the test classpath). */
    public interface FakeGrantedAuthority {
        String getAuthority();
    }

    public record FakeAuthentication(String name, List<FakeGrantedAuthority> authorities) implements Principal {
        @Override
        public String getName() {
            return name;
        }

        public Object getAuthorities() {
            return authorities;
        }
    }

    @Test
    void forwardsTheExchangePrincipalWithAuthorities() {
        ExchangePrincipalResolver resolver =
                new ExchangePrincipalResolver(FakeAuthentication.class, FakeGrantedAuthority.class);
        VesperaReactiveProxyHandler handler = handler(
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), resolver, 1024, 1024, CorsHandling.RUST);
        Principal alice = new FakeAuthentication("alice", List.of(() -> "ROLE_ADMIN", () -> "SCOPE_read"));
        WebFilter authenticate = (exchange, chain) ->
                chain.filter(exchange.mutate().principal(Mono.just(alice)).build());
        assertEquals(200, call(client(handler, ProxyPathPatterns.defaults(), true,
                ctx -> ctx.registerBean("authenticate", WebFilter.class, () -> authenticate))
                .get().uri("/me")).status());
        JsonNode principal = transport.lastHeader().get("principal");
        assertEquals("alice", principal.get("name").asText());
        assertEquals("ROLE_ADMIN", principal.get("authorities").get(0).asText());
        assertEquals("SCOPE_read", principal.get("authorities").get(1).asText());

        // No principal on the exchange: the field is omitted.
        assertEquals(200, call(client(handler).get().uri("/me")).status());
        assertNull(transport.lastHeader().get("principal"));
    }

    @Test
    void streamsEventStreamResponsesThroughTheNativeStreamingSymbol() throws Exception {
        Result result = call(client(handler()).get().uri("/events")
                .header(HttpHeaders.ACCEPT, "text/event-stream"));
        assertEquals(200, result.status());
        assertEquals("text/event-stream", result.header(HttpHeaders.CONTENT_TYPE));
        assertNull(result.header(HttpHeaders.CONTENT_LENGTH));
        assertEquals("data: a\n\ndata: b\n\n", result.body());
        assertEquals("/events", transport.lastHeader().get("path").asText());
        DispatchEvent event = awaitEvent();
        assertEquals(DispatchMode.STREAMING, event.mode());
        assertEquals(200, event.status());
    }

    @Test
    void streamedNoContentResponseHasNoBody() {
        transport.streamHeader = responseWire(204, "{}", "");
        Result result = call(client(handler()).get().uri("/events")
                .header(HttpHeaders.ACCEPT, "application/x-ndjson"));
        assertEquals(204, result.status());
        assertEquals("", result.body());
    }

    @Test
    void clientCancellationCancelsTheNativeDispatch() throws Exception {
        // Mirror the real native wiring: cancelling the future aborts native task 7.
        AtomicLong cancelledToken = new AtomicLong();
        CompletableFuture<byte[]> nativeFuture = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(nativeFuture, 7, token -> {
            cancelledToken.set(token);
            return true;
        });
        transport.pending = nativeFuture;
        VesperaReactiveProxyHandler handler = handler();
        MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.get("/slow"));
        Disposable subscription = handler.handle(exchange).subscribe();
        assertEquals(1, transport.requests.size(), "dispatched before the client went away");
        subscription.dispose(); // what reactor-netty does when the connection closes

        assertTrue(nativeFuture.isCancelled());
        assertEquals(7, cancelledToken.get());
        DispatchEvent event = awaitEvent();
        assertEquals(VesperaReactiveProxyHandler.CLIENT_CLOSED_REQUEST, event.status());
        assertEquals(AppNameResolver.DEFAULT_APP, event.app());
    }

    @Test
    void saturatedStreamingPoolAnswers503WithoutDispatching() throws Exception {
        // One streaming thread, one queue slot.
        VesperaStreamingExecutor streaming = new VesperaStreamingExecutor(1, 1);
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App", List.of()), null, 1024, 1024,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST, streaming);
        java.util.concurrent.CountDownLatch gate = new java.util.concurrent.CountDownLatch(1);
        transport.streamGate = gate;
        try {
            // The first stream holds the only thread; the second waits in the queue.
            Disposable running = handler.handle(sse("/one")).subscribe();
            assertTrue(transport.streamsEntered.tryAcquire(5, java.util.concurrent.TimeUnit.SECONDS));
            Disposable queued = handler.handle(sse("/two")).subscribe();
            assertEquals(2, streaming.inUse());

            Result rejected = call(client(handler).get().uri("/three")
                    .header(HttpHeaders.ACCEPT, "text/event-stream"));
            assertEquals(503, rejected.status());
            assertEquals(ProxyErrors.STREAMING_SATURATED, rejected.body());
            assertTrue(rejected.header("X-Request-Id") != null);
            assertEquals(1, transport.requests.size(), "never reached Rust");
            DispatchEvent event = awaitEvent();
            assertEquals(503, event.status());
            assertEquals(DispatchMode.STREAMING, event.mode());
            // Buffered requests never touch the streaming pool.
            assertEquals(200, call(client(handler).get().uri("/plain")).status());

            gate.countDown();
            long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
            while (streaming.inUse() > 0 && System.nanoTime() < deadline) {
                Thread.sleep(5);
            }
            assertEquals(0, streaming.inUse(), "slots are freed when the streams end");
            running.dispose();
            queued.dispose();
            transport.streamGate = null;
            assertEquals(200, call(client(handler).get().uri("/four")
                    .header(HttpHeaders.ACCEPT, "text/event-stream")).status());
        } finally {
            gate.countDown();
            streaming.close();
        }
    }

    @Test
    void streamCancelledWhileQueuedNeverReachesRust() throws Exception {
        // One streaming thread, one queue slot.
        VesperaStreamingExecutor streaming = new VesperaStreamingExecutor(1, 1);
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App", List.of()), null, 1024, 1024,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST, streaming);
        java.util.concurrent.CountDownLatch gate = new java.util.concurrent.CountDownLatch(1);
        transport.streamGate = gate;
        try {
            Disposable running = handler.handle(sse("/one")).subscribe();
            assertTrue(transport.streamsEntered.tryAcquire(5, java.util.concurrent.TimeUnit.SECONDS));
            // Subscribed: its task is submitted and waits in the pool queue.
            Disposable queued = handler.handle(sse("/two")).subscribe();
            assertEquals(2, streaming.inUse());

            queued.dispose(); // the client disconnected while queued
            assertEquals(1, streaming.inUse(), "the queued slot is freed at once");

            gate.countDown();
            long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
            while (streaming.inUse() > 0 && System.nanoTime() < deadline) {
                Thread.sleep(5);
            }
            assertEquals(0, streaming.inUse());
            // Give a (wrongly) surviving queued task the chance to run.
            assertFalse(transport.streamsEntered.tryAcquire(200, java.util.concurrent.TimeUnit.MILLISECONDS),
                    "the cancelled stream never runs the native dispatch");
            assertEquals(1, transport.requests.size(), "only the running stream reached Rust");
            running.dispose();
        } finally {
            gate.countDown();
            streaming.close();
        }
    }

    private static MockServerWebExchange sse(String path) {
        return MockServerWebExchange.from(MockServerHttpRequest.get(path)
                .header(HttpHeaders.ACCEPT, "text/event-stream"));
    }

    @Test
    void failuresBeforeTheResponseBecomePlainText500() {
        transport.response = new byte[] {0, 0, 0, 99, 1}; // header length beyond the buffer
        Result result = call(client(handler()).get().uri("/broken"));
        assertEquals(500, result.status());
        assertEquals(ProxyErrors.GENERIC_FAILURE, result.body());
    }
}
