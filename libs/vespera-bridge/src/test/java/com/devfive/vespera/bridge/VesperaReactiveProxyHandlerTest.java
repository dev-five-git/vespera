package com.devfive.vespera.bridge;

import static com.devfive.vespera.bridge.ReactiveTestSupport.call;
import static com.devfive.vespera.bridge.ReactiveTestSupport.responseWire;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
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
import org.springframework.core.io.buffer.DefaultDataBufferFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.test.web.reactive.server.WebTestClient;
import org.springframework.web.bind.annotation.CrossOrigin;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.cors.CorsConfiguration;
import org.springframework.web.cors.reactive.CorsWebFilter;
import org.springframework.web.cors.reactive.UrlBasedCorsConfigurationSource;
import org.springframework.web.reactive.config.CorsRegistry;
import org.springframework.web.reactive.config.EnableWebFlux;
import org.springframework.web.reactive.config.WebFluxConfigurer;
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

        @CrossOrigin(origins = "https://java.example")
        @PostMapping(value = "/java/orders", consumes = "application/json")
        String orders() {
            return "orders";
        }

        @CrossOrigin(origins = "https://java.example")
        @PostMapping(value = "/java/keyed", headers = "X-Api=1")
        String keyed() {
            return "keyed";
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

    /**
     * A body that completes normally after fewer bytes than its Content-Length
     * (the container may end it that way): Rust must not see it as a complete
     * request, and the proxy answers 400 like the servlet path. Reactor-netty
     * itself reports a half-closed request as an error, so this drives the
     * exchange directly.
     */
    @Test
    void aBodyThatEndsShortOfItsDeclaredLengthIsRefusedBeforeRust() {
        VesperaReactiveProxyHandler proxy = handler();
        MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.post("/upload")
                .header(HttpHeaders.CONTENT_LENGTH, "1000")
                .header("Accept", "application/json")
                .body(Flux.just(new DefaultDataBufferFactory().wrap(new byte[400]))));

        proxy.handle(exchange).block();
        assertEquals(HttpStatus.BAD_REQUEST, exchange.getResponse().getStatusCode());
        assertTrue(transport.requests.isEmpty(), "a short body must not reach Rust");
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

    /**
     * The event of the request to {@code path}: events are recorded after the
     * response hand-off, so one may arrive after {@code call(...)} returned,
     * and the newest event overall can still be an earlier request's.
     */
    private DispatchEvent awaitEvent(String path) throws InterruptedException {
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (System.nanoTime() < deadline) {
            for (DispatchEvent event : events) {
                if (path.equals(event.path())) {
                    return event;
                }
            }
            Thread.sleep(5);
        }
        throw new AssertionError("no DispatchEvent for " + path + " in " + events);
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
    void pathParametersAreStrippedLikeTheServletProxy() throws Exception {
        Result result = call(client(handler()).get()
                .uri(URI.create("http://localhost/users/42;jsessionid=abc/orders;v=1;w=2?x=1")));
        assertEquals(200, result.status());
        assertEquals("/users/42/orders?x=1", transport.lastHeader().get("path").asText());
        assertEquals("/users/42/orders", awaitEvent().path());
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
    void unlimitedCapNamesTheBuiltInBoundNotAnUnsetProperty() {
        String unlimited = VesperaReactiveProxyHandler.bodyTooLargeMessage(0, 64L * 1024 * 1024);
        assertTrue(unlimited.contains("built-in 67108864-byte bound"), unlimited);
        assertTrue(unlimited.contains("max-buffered-request-bytes=0"), unlimited);
        assertFalse(unlimited.contains("max-buffered-request-bytes=67108864"), unlimited);
        assertEquals("buffered request body exceeds vespera.bridge.max-buffered-request-bytes=1024",
                VesperaReactiveProxyHandler.bodyTooLargeMessage(1024, 1024));
        // A cap above the single-array bound was clamped: the bound applied,
        // the reason names it and the cap as configured.
        long bound = Integer.MAX_VALUE - 8L;
        String clamped = VesperaReactiveProxyHandler.bodyTooLargeMessage(4294967296L, bound);
        assertEquals("buffered request body exceeds the built-in " + bound
                + "-byte bound (vespera.bridge.max-buffered-request-bytes=4294967296)", clamped);
    }

    @Test
    void declaredLengthAboveTheSingleArrayBoundNamesTheBoundNotAnUnsetProperty() {
        long bound = VesperaWireCodec.MAX_FRAME_BODY;
        for (long cap : new long[] {0, 4294967296L}) {
            Result result = call(client(handler(new ReactiveHeaderAppNameResolver("X-Vespera-App", null),
                    null, cap, 1024, CorsHandling.RUST)).post().uri("/upload")
                    .header(HttpHeaders.CONTENT_LENGTH, "3221225472"));
            assertEquals(413, result.status());
            assertEquals("buffered request body reaches the built-in " + bound
                    + "-byte single-array bound (vespera.bridge.max-buffered-request-bytes=" + cap
                    + "; actual 3221225472 bytes)", result.body());
            assertFalse(result.body().contains("max-buffered-request-bytes=" + bound), result.body());
        }
        assertTrue(transport.requests.isEmpty());
    }

    @Test
    void declaredLengthAboveTheWireFrameBoundIsRejectedUpfront() {
        // Servlet parity: a declared body whose wire frame (with the header
        // reserve) cannot fit a byte[] is a 413 before any aggregation, so no
        // body is read only to be refused.
        long frameBound = VesperaWireCodec.MAX_FRAME_BODY;
        for (long declared : new long[] {frameBound + 1, Integer.MAX_VALUE - 8L}) {
            Result result = call(client(handler(new ReactiveHeaderAppNameResolver("X-Vespera-App", null),
                    null, 0, 1024, CorsHandling.RUST)).post().uri("/upload")
                    .header(HttpHeaders.CONTENT_LENGTH, Long.toString(declared)));
            assertEquals(413, result.status(), Long.toString(declared));
            assertEquals("buffered request body reaches the built-in " + frameBound
                    + "-byte single-array bound (vespera.bridge.max-buffered-request-bytes=0; actual "
                    + declared + " bytes)", result.body());
        }
        assertTrue(transport.requests.isEmpty());
    }

    @Test
    void requestBodyOverTheCapIs413() throws Exception {
        WebTestClient client = client(handler());
        // Declared Content-Length over the cap: rejected before reading.
        assertEquals(413, call(client.post().uri("/upload/declared").bodyValue("x".repeat(2000))).status());
        // Unknown length (streamed body): rejected while aggregating. Its own
        // path, so its event is told apart from the declared request's.
        Result streamed = call(client.post().uri("/upload/streamed")
                .body(Flux.range(0, 40).map(i -> "y".repeat(100)), String.class));
        assertEquals(413, streamed.status());
        assertTrue(streamed.body().contains("max-buffered-request-bytes=1024"), streamed.body());
        assertTrue(transport.requests.isEmpty());
        assertEquals(413, awaitEvent("/upload/streamed").status());
        // At the cap: dispatched.
        assertEquals(200, call(client.post().uri("/upload").bodyValue("z".repeat(1024))).status());
        assertEquals(1024, transport.lastBody().length());
    }

    @Test
    void responseBodyOverTheCapIs500() {
        transport.response = responseWire(200, "{}", "r".repeat(1025));
        Result result = call(client(handler()).get().uri("/big").header(HttpHeaders.ACCEPT, "application/json"));
        assertEquals(500, result.status());
        assertTrue(result.body().contains("max-buffered-response-bytes=1024"), result.body());

        // An implicitly streamed GET is not buffered past the cap: it streams.
        Result streamed = call(client(handler()).get().uri("/big"));
        assertEquals(200, streamed.status());
        assertEquals("r".repeat(1025), streamed.body());
        assertNull(streamed.header(HttpHeaders.CONTENT_LENGTH));
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
    void preflightForAJavaEndpointWithConsumesOrHeadersConditionsIsAnsweredBySpring() {
        WebTestClient client = client(handler());
        for (String path : List.of("/java/orders", "/java/keyed")) {
            Result allowed = call(client.options().uri(URI.create("http://localhost" + path))
                    .header(HttpHeaders.ORIGIN, "https://java.example")
                    .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(HttpHeaders.ACCESS_CONTROL_REQUEST_HEADERS, "content-type, x-api"));
            assertEquals(200, allowed.status(), path);
            assertEquals("https://java.example", allowed.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN), path);
        }
        assertTrue(transport.requests.isEmpty(), "never reached Rust");
    }

    @Test
    void withoutThePreflightFilterSpringKeepsPreflights() {
        Result preflight = call(client(handler(), ProxyPathPatterns.defaults(), false, ctx -> { })
                .options().uri("/users")
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        // Spring's own answer: no CORS config, so no Allow-Origin header. The status is
        // Framework-specific (403 on 6.2 under testBoot3, 200 on 7), never Rust's and never a 500.
        assertTrue(List.of(200, 403).contains(preflight.status()), "status " + preflight.status());
        assertNull(preflight.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        assertTrue(transport.requests.isEmpty(), "DispatcherHandler never runs a handler for a preflight");
    }

    @Test
    void aBlankRequestMethodPreflightIsAnsweredByTheProxy() {
        // An empty Access-Control-Request-Method used to throw from the filter (a 500).
        // With no actual request to match, the proxy owns the preflight, as it does
        // when no mapping exists: it is dispatched to Rust, not answered by Spring.
        Result preflight = call(client(handler()).options().uri("/users")
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, ""));
        assertNotEquals(500, preflight.status());
        assertEquals(1, transport.requests.size(), "answered by the proxy, not an error");
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

    /** {@code WebFluxConfigurer#addCorsMappings}: global CORS for {@code /**}. */
    private static Consumer<AnnotationConfigApplicationContext> globalCorsMappings() {
        return ctx -> ctx.registerBean("globalCors", WebFluxConfigurer.class, () -> new WebFluxConfigurer() {
            @Override
            public void addCorsMappings(CorsRegistry registry) {
                registry.addMapping("/**").allowedOrigins("https://spring.example").allowedMethods("*");
            }
        });
    }

    @Test
    void springCorsAppliesGlobalCorsMappingsToProxiedPaths() {
        transport.response = responseWire(200, "{\"access-control-allow-origin\":\"*\"}", "ok");
        VesperaReactiveProxyHandler springOwned = handler(
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1024, 1024, CorsHandling.SPRING);
        WebTestClient client = client(springOwned, ProxyPathPatterns.defaults(), false, globalCorsMappings());

        Result preflight = call(client.options().uri(URI.create("http://localhost/users"))
                .header(HttpHeaders.ORIGIN, "https://spring.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertEquals(200, preflight.status());
        assertEquals("https://spring.example", preflight.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        assertEquals("PUT", preflight.header(HttpHeaders.ACCESS_CONTROL_ALLOW_METHODS));

        Result foreign = call(client.options().uri(URI.create("http://localhost/users"))
                .header(HttpHeaders.ORIGIN, "https://evil.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertEquals(403, foreign.status(), "Spring applies the global CORS policy");
        assertTrue(transport.requests.isEmpty(), "preflights never reach Rust with cors=spring");

        Result actual = call(client.get().uri(URI.create("http://localhost/c"))
                .header(HttpHeaders.ORIGIN, "https://spring.example"));
        assertEquals(200, actual.status());
        assertEquals(List.of("https://spring.example"),
                actual.headers().getValuesAsList(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));

        Result rejected = call(client.get().uri(URI.create("http://localhost/c"))
                .header(HttpHeaders.ORIGIN, "https://evil.example"));
        assertEquals(403, rejected.status());
        assertEquals(1, transport.requests.size(), "a rejected cross-origin request is not dispatched");
    }

    @Test
    void rustCorsIgnoresGlobalCorsMappings() {
        transport.response = responseWire(204,
                "{\"access-control-allow-origin\":\"https://a.example\"}", "");
        WebTestClient client = client(handler(), ProxyPathPatterns.defaults(), true, globalCorsMappings());
        Result preflight = call(client.options().uri(URI.create("http://localhost/users"))
                .header(HttpHeaders.ORIGIN, "https://a.example")
                .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, "PUT"));
        assertEquals(204, preflight.status());
        assertEquals("https://a.example", preflight.header(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        assertEquals(1, transport.requests.size());

        transport.response = responseWire(200, "{\"access-control-allow-origin\":\"*\"}", "ok");
        Result actual = call(client.get().uri(URI.create("http://localhost/c"))
                .header(HttpHeaders.ORIGIN, "https://a.example"));
        assertEquals(200, actual.status(), "no Spring CORS check on Rust-owned CORS");
        assertEquals(List.of("*"), actual.headers().getValuesAsList(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
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

    /** An event stream fetched with {@code Accept: *}{@code /*} (or none) still streams. */
    @Test
    void bodylessGetWithAWildcardAcceptStreamsAnEventStream() throws Exception {
        transport.streamImplicitRequests = true;
        String[] accepts = {null, "*/*", "text/*;q=0.5, application/json"};
        for (int i = 0; i < accepts.length; i++) {
            // A distinct path per request: each event is checked against its own
            // request, not against an earlier one that is still in `events`.
            String path = "/events/" + i;
            WebTestClient.RequestHeadersSpec<?> spec = client(handler()).get().uri(path);
            if (accepts[i] != null) {
                spec = spec.header(HttpHeaders.ACCEPT, accepts[i]);
            }
            Result result = call(spec);
            assertEquals(200, result.status(), String.valueOf(accepts[i]));
            assertEquals("text/event-stream", result.header(HttpHeaders.CONTENT_TYPE));
            assertEquals("data: a\n\ndata: b\n\n", result.body());
            assertNull(result.header(HttpHeaders.CONTENT_LENGTH), "streamed, not buffered");
            assertEquals(DispatchMode.STREAMING.name(), awaitEvent(path).mode().name(), path);
        }
        assertTrue(transport.futures.isEmpty(), "never the buffered async path");
    }

    /**
     * RFC 9110 section 5.3: two Accept field lines are one comma-joined value.
     * The streaming range on the second line must still take the streaming path
     * (the first line alone used to route the request through buffered ASYNC).
     */
    @Test
    void aStreamingAcceptOnALaterFieldLineStreamsTheResponse() throws Exception {
        Result result = call(client(handler()).get().uri("/events/split-lines")
                .headers(headers -> {
                    headers.add(HttpHeaders.ACCEPT, "application/json");
                    headers.add(HttpHeaders.ACCEPT, "text/event-stream");
                }));
        assertEquals(200, result.status());
        assertEquals("text/event-stream", result.header(HttpHeaders.CONTENT_TYPE));
        assertTrue(transport.futures.isEmpty(), "streamed, never the buffered async path");
        assertEquals(DispatchMode.STREAMING.name(), awaitEvent("/events/split-lines").mode().name());
    }

    /** An ordinary response of an implicitly streamed GET is held back and keeps its Content-Length. */
    @Test
    void implicitlyStreamedGetKeepsTheContentLengthOfAnInMemoryBody() throws Exception {
        transport.response = responseWire(200, "{\"content-type\":\"application/json\"}", "{\"id\":1}");
        Result result = call(client(handler()).get().uri("/users/1"));
        assertEquals(200, result.status());
        assertEquals("{\"id\":1}", result.body());
        assertEquals("8", result.header(HttpHeaders.CONTENT_LENGTH));
        assertTrue(transport.futures.isEmpty());
    }

    /** A concrete non-streaming Accept, a HEAD or a request body keep the buffered ASYNC path. */
    @Test
    void nonStreamingAcceptHeadAndBodiesStayBuffered() throws Exception {
        call(client(handler()).get().uri("/users").header(HttpHeaders.ACCEPT, "application/json"));
        call(client(handler()).get().uri("/users").header(HttpHeaders.ACCEPT, "text/event-stream;q=0, */*;q=0"));
        call(client(handler()).head().uri("/users"));
        call(client(handler()).post().uri("/users").bodyValue("x"));
        assertEquals(4, transport.futures.size());
    }

    /** A saturated streaming pool sends an implicit candidate down the buffered path (never 503). */
    @Test
    void saturatedStreamingPoolFallsBackToBufferedForImplicitStreams() throws Exception {
        VesperaStreamingExecutor streaming = new VesperaStreamingExecutor(1, 0);
        streaming.close(); // admits nothing
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST, streaming);
        Result result = call(client(handler).get().uri("/users"));
        assertEquals(200, result.status());
        assertEquals("ok", result.body());
        assertEquals(1, transport.futures.size());
        assertEquals(DispatchMode.ASYNC.name(), awaitEvent().mode().name());
        assertEquals(503, call(client(handler).get().uri("/events")
                .header(HttpHeaders.ACCEPT, "text/event-stream")).status(), "an explicit stream still 503s");
    }

    /**
     * No streaming thread can start (thread exhaustion): the pool refuses the
     * task, which is treated as saturation (explicit stream {@code 503},
     * implicit one buffered), not a generic proxy {@code 500}; Rust never runs
     * the stream and the slot is freed.
     */
    @Test
    void aStreamingThreadThatCannotStartIsTreatedAsSaturation() throws Exception {
        VesperaStreamingExecutor streaming = new VesperaStreamingExecutor(1, 1,
                java.util.concurrent.TimeUnit.SECONDS.toNanos(60), runnable -> {
                    throw new OutOfMemoryError("unable to create native thread");
                });
        try (streaming) {
            VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                    new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
                    "X-Request-Id", List.of(events::add), CorsHandling.RUST, streaming);
            Result explicit = call(client(handler).get().uri("/events")
                    .header(HttpHeaders.ACCEPT, "text/event-stream"));
            assertEquals(503, explicit.status());
            assertTrue(explicit.body().contains(ProxyErrors.STREAMING_SATURATED), explicit.body());
            assertEquals(0, streaming.inUse());
            assertEquals(503, awaitEvent().status());
            events.clear();
            Result implicit = call(client(handler).get().uri("/users"));
            assertEquals(200, implicit.status());
            assertEquals("ok", implicit.body());
            assertEquals(1, transport.futures.size(), "the implicit stream fell back to buffered");
            assertEquals(DispatchMode.ASYNC.name(), awaitEvent().mode().name());
            assertEquals(0, streaming.inUse());
        }
    }

    @Test
    void spillingGateHoldsUntilTheFlushHintOrTheLimit() throws Exception {
        java.io.ByteArrayOutputStream sink = new java.io.ByteArrayOutputStream();
        MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.get("/x"));
        ReactiveProxyResponse out = new ReactiveProxyResponse(exchange.getResponse(), null, null, null);
        byte[] header = responseWire(200, "{\"content-type\":\"text/plain\"}", "");
        int headerLen = java.nio.ByteBuffer.wrap(header).getInt();

        VesperaReactiveProxyHandler.SpillingGate gate =
                new VesperaReactiveProxyHandler.SpillingGate(sink, out, "GET", 4);
        gate.onHeader(out.applyWireHeader(header, 4, headerLen, true), "GET");
        gate.write("abc".getBytes(), 0, 3);
        assertEquals(0, sink.size(), "held back");
        gate.flush(); // the native "body would wait" hint
        assertFalse(gate.holding());
        assertEquals("abc", sink.toString());
        gate.write("de".getBytes(), 0, 2);
        assertEquals("abcde", sink.toString(), "pass-through once spilled");
        gate.finish();
        assertNull(exchange.getResponse().getHeaders().getFirst(HttpHeaders.CONTENT_LENGTH));

        java.io.ByteArrayOutputStream big = new java.io.ByteArrayOutputStream();
        VesperaReactiveProxyHandler.SpillingGate limited =
                new VesperaReactiveProxyHandler.SpillingGate(big, out, "GET", 4);
        limited.onHeader(out.applyWireHeader(header, 4, headerLen, true), "GET");
        limited.write("12345".getBytes(), 0, 5);
        assertEquals("12345", big.toString(), "past the limit: streamed");

        MockServerWebExchange small = MockServerWebExchange.from(MockServerHttpRequest.get("/y"));
        ReactiveProxyResponse smallOut = new ReactiveProxyResponse(small.getResponse(), null, null, null);
        java.io.ByteArrayOutputStream whole = new java.io.ByteArrayOutputStream();
        VesperaReactiveProxyHandler.SpillingGate held =
                new VesperaReactiveProxyHandler.SpillingGate(whole, smallOut, "GET", 1024);
        held.onHeader(smallOut.applyWireHeader(header, 4, headerLen, true), "GET");
        held.write('h');
        held.write("ey".getBytes(), 0, 2);
        assertEquals(0, whole.size());
        held.finish();
        assertEquals("hey", whole.toString());
        assertEquals(3, small.getResponse().getHeaders().getContentLength());
    }

    /** Records the arrays handed to {@code write}, to prove a write was not copied. */
    private static final class RecordingSink extends java.io.OutputStream {
        final List<byte[]> arrays = new ArrayList<>();
        final java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream();

        @Override
        public void write(int b) {
            bytes.write(b);
        }

        @Override
        public void write(byte[] b, int off, int len) {
            arrays.add(b);
            bytes.write(b, off, len);
        }
    }

    /**
     * A write past the hold limit goes straight to the delegate (the same
     * array, no copy through the hold buffer), and a {@code SpillingGate}'s
     * {@code pending()} reports its real state (it used to report the unused
     * inherited buffer: always {@code true}).
     */
    @Test
    void gatesPassALargeWriteThroughUncopiedAndReportTheirRealState() throws Exception {
        MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.get("/x"));
        ReactiveProxyResponse out = new ReactiveProxyResponse(exchange.getResponse(), null, null, null);
        byte[] header = responseWire(200, "{\"content-type\":\"text/plain\"}", "");
        int headerLen = java.nio.ByteBuffer.wrap(header).getInt();

        RecordingSink spilled = new RecordingSink();
        VesperaReactiveProxyHandler.SpillingGate gate =
                new VesperaReactiveProxyHandler.SpillingGate(spilled, out, "GET", 4);
        gate.onHeader(out.applyWireHeader(header, 4, headerLen, true), "GET");
        assertTrue(gate.pending());
        gate.write("ab".getBytes(), 0, 2);
        byte[] large = "cdefgh".getBytes();
        gate.write(large, 1, 5);
        assertEquals("abdefgh", spilled.bytes.toString());
        assertSame(large, spilled.arrays.get(spilled.arrays.size() - 1), "passed through uncopied");
        assertFalse(gate.holding());
        assertFalse(gate.pending(), "pending() mirrors the spilling gate's state");

        RecordingSink released = new RecordingSink();
        VesperaReactiveProxyHandler.BodyGate body = new VesperaReactiveProxyHandler.BodyGate(released);
        byte[] chunk = new byte[VesperaReactiveProxyHandler.STREAM_CHUNK_BYTES + 1];
        body.write(chunk, 0, chunk.length);
        assertSame(chunk, released.arrays.get(0), "passed through uncopied");
        assertFalse(body.pending());
        assertEquals(chunk.length, released.bytes.size());
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
        // A concrete non-streaming Accept: the buffered (cancellable) ASYNC path.
        MockServerWebExchange exchange = MockServerWebExchange.from(
                MockServerHttpRequest.get("/slow").header(HttpHeaders.ACCEPT, "application/json"));
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
                new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
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
            DispatchEvent event = awaitEvent("/three");
            assertEquals(503, event.status());
            assertEquals(DispatchMode.STREAMING, event.mode());
            // Buffered requests never touch the streaming pool: a concrete
            // non-streaming Accept (a bodyless GET with no / wildcard Accept
            // would be an implicit-streaming candidate and try the pool).
            long attempts = streaming.reserveAttempts();
            assertEquals(200, call(client(handler).get().uri("/plain")
                    .header(HttpHeaders.ACCEPT, "application/json")).status());
            assertEquals(attempts, streaming.reserveAttempts(), "never asked the streaming pool");
            assertEquals(2, streaming.inUse());
            assertEquals(DispatchMode.ASYNC, awaitEvent("/plain").mode(), "served buffered, not streamed");
            // The probe is live: an implicit-streaming candidate does ask the
            // (saturated) pool, then falls back to buffered.
            assertEquals(200, call(client(handler).get().uri("/implicit")).status());
            assertEquals(attempts + 1, streaming.reserveAttempts());
            assertEquals(DispatchMode.ASYNC, awaitEvent("/implicit").mode());

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
                new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
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
        // A concrete non-streaming Accept: the buffered ASYNC decode path (a
        // bodyless GET without one takes the implicit-streaming path).
        Result result = call(client(handler()).get().uri("/broken")
                .header(HttpHeaders.ACCEPT, "application/json"));
        assertEquals(500, result.status());
        assertEquals(ProxyErrors.GENERIC_FAILURE, result.body());
        assertEquals(1, transport.requests.size());
        assertEquals(1, transport.futures.size(), "the buffered ASYNC path, not a stream");
    }
}
