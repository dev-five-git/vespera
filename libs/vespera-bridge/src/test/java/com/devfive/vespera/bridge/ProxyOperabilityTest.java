package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Test;
import org.slf4j.LoggerFactory;
import org.slf4j.MDC;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * Request ids, client address forwarding, proxy-written errors and the
 * {@link DispatchObserver} SPI — everything that does not need a live
 * native library.
 */
class ProxyOperabilityTest {

    // ── request id ─────────────────────────────────────────────────────

    @Test
    void requestIdKeepsSaneClientTokensAndOtherwiseGeneratesAUuid() {
        assertEquals("abc-123", VesperaProxyController.resolveRequestId("abc-123"));
        for (String bad : new String[] {null, "", "has space", "x".repeat(129), "tab\t", "é"}) {
            String generated = VesperaProxyController.resolveRequestId(bad);
            UUID uuid = UUID.fromString(generated);
            assertEquals(4, uuid.version(), generated);
            assertEquals(2, uuid.variant(), generated);
        }
        assertNotEquals(VesperaProxyController.resolveRequestId(null),
                VesperaProxyController.resolveRequestId(null));
        assertTrue(VesperaProxyController.isValidRequestId("x".repeat(128)));
    }

    @Test
    void responseWrapperEchoesTheIdUnlessRustSetsOne() {
        MockHttpServletResponse raw = new MockHttpServletResponse();
        ProxyServletResponse response = new ProxyServletResponse(raw, "X-Request-Id", "gen-1");
        response.applyDefaultRequestId();
        assertEquals("gen-1", raw.getHeader("X-Request-Id"));

        response.addHeader("x-request-id", "rust-1");
        response.addHeader("x-request-id", "rust-2");
        response.addHeader("x-other", "o");
        assertEquals(List.of("rust-1", "rust-2"), raw.getHeaders("X-Request-Id"));
        assertEquals("o", raw.getHeader("x-other"));

        response.reset();
        assertEquals(List.of("gen-1"), raw.getHeaders("X-Request-Id"), "reset keeps the echo");
        response.setHeader("X-Request-Id", "rust-3");
        assertEquals(List.of("rust-3"), raw.getHeaders("X-Request-Id"));
        response.applyDefaultRequestId();
        assertEquals(List.of("rust-3"), raw.getHeaders("X-Request-Id"), "upstream id wins");

        ProxyServletResponse disabled = new ProxyServletResponse(raw, "X-Request-Id", null);
        disabled.applyDefaultRequestId();
        disabled.reset();
        assertNull(disabled.requestIdHeader());
        assertFalse(raw.containsHeader("X-Request-Id"));

        ProxyServletResponse.markPanicked(raw); // not a wrapper: ignored
        ProxyServletResponse.markPanicked(response);
        assertTrue(response.panicked());
    }

    // ── forwarded headers + connection facts ───────────────────────────

    private static Map<String, String> collect(VesperaBridge.HeaderSource source) {
        Map<String, String> out = new LinkedHashMap<>();
        source.writeTo(out::put);
        return out;
    }

    @Test
    void forwardedHeadersReplaceTheRequestIdAndCarryTheClientAddress() {
        MockHttpServletRequest request = new MockHttpServletRequest("POST", "/users");
        request.addHeader("X-Request-Id", "bad id");
        request.addHeader("X-Vespera-App", "admin");
        request.addHeader("Content-Length", "10");
        request.addHeader("Accept", "text/plain");
        request.setRemoteAddr("0:0:0:0:0:0:0:1");
        request.setRemotePort(51000);
        request.setScheme("HTTPS");

        ProxyHeaderSource source = new ProxyHeaderSource(
                request, "x-vespera-app", "3", "x-request-id", "id-1", VesperaPrincipal.of("alice"));
        Map<String, String> headers = collect(source);
        assertEquals("id-1", headers.get("x-request-id"));
        assertEquals("3", headers.get("content-length"));
        assertEquals("text/plain", headers.get("accept"));
        assertFalse(headers.containsKey("x-vespera-app"));
        assertEquals("[0:0:0:0:0:0:0:1]:51000", source.remoteAddr());
        assertEquals("https", source.scheme());
        assertEquals(VesperaPrincipal.of("alice"), source.principal());
        assertNull(VesperaProxyController.requestHeaders(request, null, null).principal());

        Map<String, String> plain = collect(VesperaProxyController.requestHeaders(request, null, null));
        assertEquals("bad id", plain.get("x-request-id"));
    }

    @Test
    void connectionFactsFollowTheContainerView() {
        MockHttpServletRequest request = new MockHttpServletRequest();
        request.setRemoteAddr("203.0.113.9");
        request.setRemotePort(-1);
        assertEquals("203.0.113.9:0", ProxyHeaderSource.remoteAddr(request));
        request.setRemoteAddr("[::1]");
        request.setRemotePort(8);
        assertEquals("[::1]:8", ProxyHeaderSource.remoteAddr(request));
        request.setRemoteAddr(null);
        assertNull(ProxyHeaderSource.remoteAddr(request));
        request.setRemoteAddr("");
        assertNull(ProxyHeaderSource.remoteAddr(request));

        request.setScheme("http");
        assertEquals("http", ProxyHeaderSource.scheme(request));
        request.setScheme("ws");
        request.setSecure(true);
        assertEquals("https", ProxyHeaderSource.scheme(request));
        request.setSecure(false);
        assertEquals("http", ProxyHeaderSource.scheme(request));
    }

    @Test
    void wireHeaderCarriesRemoteAddrAndSchemeOnlyWhenPresent() {
        VesperaBridge.HeaderSource withConn = new VesperaBridge.HeaderSource() {
            @Override
            public void writeTo(VesperaBridge.HeaderSink sink) {
                sink.put("accept", "*/*");
            }

            @Override
            public String remoteAddr() {
                return "192.0.2.1:443";
            }

            @Override
            public String scheme() {
                return "https";
            }
        };
        assertEquals("{\"v\":1,\"method\":\"GET\",\"path\":\"/p\",\"headers\":{\"accept\":\"*/*\"},"
                        + "\"remote_addr\":\"192.0.2.1:443\",\"scheme\":\"https\",\"app\":\"a\"}",
                headerJson(VesperaBridge.encodeRequest("a", "GET", "/p", "", withConn, null)));

        VesperaBridge.HeaderSource bare = sink -> { };
        assertNull(bare.remoteAddr());
        assertNull(bare.scheme());
        assertEquals("{\"v\":1,\"method\":\"GET\",\"path\":\"/p\"}",
                headerJson(VesperaBridge.encodeRequest("GET", "/p", "", bare, null)));
    }

    private static String headerJson(byte[] wire) {
        int len = VesperaWireCodec.readHeaderLength(wire);
        return new String(wire, 4, len, StandardCharsets.UTF_8);
    }

    // ── proxy-written errors ────────────────────────────────────────────

    @Test
    void proxyErrorsReplaceTheResponseWithPlainText() throws IOException {
        MockHttpServletResponse response = new MockHttpServletResponse();
        // Set before the proxy (a filter): kept, unlike a full reset().
        response.setHeader("Access-Control-Allow-Origin", "https://app.example");
        response.getOutputStream().write("partial".getBytes(StandardCharsets.UTF_8));
        assertTrue(ProxyErrors.write(response, 413, "too big"));
        assertEquals(413, response.getStatus());
        assertEquals("too big", response.getContentAsString());
        assertEquals(7, response.getContentLength());
        assertEquals("https://app.example", response.getHeader("Access-Control-Allow-Origin"));
        assertTrue(response.getContentType().startsWith("text/plain"));

        response.setCommitted(true);
        assertFalse(ProxyErrors.write(response, 500, "late"));
        assertEquals(413, response.getStatus());

        ResponseEntity<byte[]> entity = ProxyErrors.entity(503, null);
        assertEquals(503, entity.getStatusCode().value());
        assertArrayEquals(new byte[0], entity.getBody());
        // Exception messages never reach the client (they go to the WARN log).
        assertEquals("vespera dispatch failed",
                ProxyErrors.describe(new RuntimeException("jdbc:postgresql://db:5432 secret")));
    }

    @Test
    void proxyErrorsDropOnlyTheHeadersTheProxyWrote() throws IOException {
        MockHttpServletResponse raw = new MockHttpServletResponse();
        raw.setHeader("Access-Control-Allow-Origin", "https://app.example");
        raw.addHeader("Set-Cookie", "f=1");
        ProxyServletResponse response = new ProxyServletResponse(raw, "X-Request-Id", "id-1");
        response.applyDefaultRequestId();
        // Rust headers applied, then the proxy fails before committing.
        response.addHeader("set-cookie", "rust=1");
        response.addHeader("content-disposition", "attachment");
        response.setHeader("x-rust", "1");

        assertTrue(ProxyErrors.write(response, 500, ProxyErrors.GENERIC_FAILURE));

        assertEquals(500, raw.getStatus());
        assertEquals("vespera dispatch failed", raw.getContentAsString());
        assertEquals("https://app.example", raw.getHeader("Access-Control-Allow-Origin"));
        assertEquals(List.of("f=1"), raw.getHeaders("Set-Cookie"), "the filter cookie stays, the Rust one goes");
        assertFalse(raw.containsHeader("content-disposition"));
        assertFalse(raw.containsHeader("x-rust"));
        assertEquals("id-1", raw.getHeader("X-Request-Id"));
    }

    @Test
    void corsHeadersAreDeduplicatedPerOwner() {
        MockHttpServletResponse springSet = new MockHttpServletResponse();
        springSet.setHeader("Access-Control-Allow-Origin", "https://spring.example");
        ProxyServletResponse rustOwned =
                new ProxyServletResponse(springSet, null, null, CorsHandling.RUST);
        rustOwned.addHeader("access-control-allow-origin", "https://rust.example");
        rustOwned.addHeader("access-control-expose-headers", "a");
        rustOwned.addHeader("access-control-expose-headers", "b");
        assertEquals(List.of("https://rust.example"), springSet.getHeaders("Access-Control-Allow-Origin"));
        assertEquals(List.of("a", "b"), springSet.getHeaders("Access-Control-Expose-Headers"));

        MockHttpServletResponse springSet2 = new MockHttpServletResponse();
        springSet2.setHeader("Access-Control-Allow-Origin", "https://spring.example");
        ProxyServletResponse springOwned =
                new ProxyServletResponse(springSet2, null, null, CorsHandling.SPRING);
        springOwned.addHeader("access-control-allow-origin", "https://rust.example");
        springOwned.setHeader("Access-Control-Allow-Origin", "https://rust2.example");
        springOwned.addHeader("access-control-max-age", "60");
        assertEquals(List.of("https://spring.example"), springSet2.getHeaders("Access-Control-Allow-Origin"));
        assertEquals("60", springSet2.getHeader("Access-Control-Max-Age"), "Rust-only CORS headers pass");

        MockHttpServletResponse plain = new MockHttpServletResponse();
        plain.setHeader("Access-Control-Allow-Origin", "https://spring.example");
        new ProxyServletResponse(plain, null, null).addHeader("Access-Control-Allow-Origin", "x");
        assertEquals(2, plain.getHeaders("Access-Control-Allow-Origin").size(), "no mode: untouched");
    }

    @Test
    void asyncEntityCorsHeadersAreDeduplicatedToo() {
        org.springframework.http.HttpHeaders entity = new org.springframework.http.HttpHeaders();
        entity.add("access-control-allow-origin", "https://rust.example");
        entity.add("content-type", "text/plain");

        MockHttpServletResponse spring = new MockHttpServletResponse();
        spring.setHeader("Access-Control-Allow-Origin", "https://spring.example");
        org.springframework.http.HttpHeaders kept = new ProxyServletResponse(
                spring, null, null, CorsHandling.SPRING).dedupeCorsHeaders(entity);
        assertNull(kept.getFirst("access-control-allow-origin"));
        assertEquals("text/plain", kept.getFirst("content-type"));

        MockHttpServletResponse rust = new MockHttpServletResponse();
        rust.setHeader("Access-Control-Allow-Origin", "https://spring.example");
        org.springframework.http.HttpHeaders moved = new ProxyServletResponse(
                rust, null, null, CorsHandling.RUST).dedupeCorsHeaders(entity);
        assertNull(moved.getFirst("access-control-allow-origin"));
        assertEquals(List.of("https://rust.example"), rust.getHeaders("Access-Control-Allow-Origin"));

        assertSame(entity, new ProxyServletResponse(new MockHttpServletResponse(), null, null,
                CorsHandling.RUST).dedupeCorsHeaders(entity), "no clash: unchanged");
        assertSame(entity, new ProxyServletResponse(rust, null, null).dedupeCorsHeaders(entity));
    }

    @Test
    void appTagsNeverReportUnknownClientSelectedNames() {
        AppTags tags = new AppTags(java.util.Set.of("public"));
        assertEquals("_default", tags.tag(null, 200, false, true));
        assertEquals("public", tags.tag("public", 404, false, false), "allowlisted");
        // Answers of the Rust app resolution itself never teach a name.
        assertEquals(AppTags.UNKNOWN, tags.tag("random-1", 404, false, true));
        assertEquals(AppTags.UNKNOWN, tags.tag("bad name!", 400, false, true));
        assertEquals(AppTags.UNKNOWN, tags.tag("x", 503, false, true));
        // Not dispatched (rejected / failed before Rust): never learned.
        assertEquals(AppTags.UNKNOWN, tags.tag("admin", 200, false, false));
        // A handler answer proves the app is registered.
        assertEquals("admin", tags.tag("admin", 401, false, true));
        assertEquals("admin", tags.tag("admin", 404, false, false), "learned");
        assertEquals("panicky", tags.tag("panicky", 500, true, true));
        for (int i = 0; i < AppTags.MAX_LEARNED + 10; i++) {
            tags.tag("app-" + i, 200, false, true);
        }
        assertEquals(AppTags.UNKNOWN, tags.tag("one-too-many", 200, false, true), "learned set is capped");
        assertTrue(AppTags.answeredByHandler(204, false));
        assertFalse(AppTags.answeredByHandler(413, false));
        assertFalse(AppTags.answeredByHandler(431, false));
        assertFalse(AppTags.answeredByHandler(408, false));
        assertTrue(AppTags.answeredByHandler(422, false));
        assertFalse(AppTags.answeredByHandler(99, false));
    }

    @Test
    void randomAppHeadersDoNotGrowTheMeterRegistry() throws Exception {
        io.micrometer.core.instrument.simple.SimpleMeterRegistry registry =
                new io.micrometer.core.instrument.simple.SimpleMeterRegistry();
        VesperaProxyController unrestricted = new VesperaProxyController(
                new HeaderAppNameResolver("X-Vespera-App"),
                request -> DispatchMode.SYNC, Runnable::run, true, 0, 0, "X-Request-Id",
                List.of(new MicrometerDispatchObserver(registry)));
        for (int i = 0; i < 50; i++) {
            MockHttpServletRequest request = new MockHttpServletRequest("GET", "/x");
            request.addHeader("X-Vespera-App", "attacker-" + i);
            // No native library in the unit suite: answered 503 before dispatch.
            unrestricted.proxy(request, new MockHttpServletResponse());
        }
        assertEquals(1, registry.getMeters().size(), registry.getMeters().toString());
        assertEquals(50, registry.get(MicrometerDispatchObserver.METER_NAME)
                .tags("app", AppTags.UNKNOWN, "status", "503").timer().count());
    }

    @Test
    void uninitializedNativeLibraryIsACleanServiceUnavailable() throws Exception {
        RecordingObserver observer = new RecordingObserver();
        VesperaProxyController controller = controller(request -> DispatchMode.SYNC, observer);
        MockHttpServletResponse response = new MockHttpServletResponse();

        assertNull(controller.proxy(new MockHttpServletRequest("GET", "/x"), response));

        assertFalse(VesperaBridge.isInitialized());
        assertEquals(503, response.getStatus());
        assertEquals(ProxyErrors.NOT_INITIALIZED, response.getContentAsString());
        assertEquals(503, observer.events.get(0).status());
    }

    @Test
    void timerCacheIsReusedAndStatusTagIsBounded() {
        io.micrometer.core.instrument.simple.SimpleMeterRegistry registry =
                new io.micrometer.core.instrument.simple.SimpleMeterRegistry();
        MicrometerDispatchObserver observer = new MicrometerDispatchObserver(registry);
        for (int i = 0; i < 3; i++) {
            observer.onComplete(new DispatchEvent("_default", DispatchMode.SYNC, "GET", "/", 200, 1, false, null));
        }
        observer.onComplete(new DispatchEvent("_default", DispatchMode.SYNC, "GET", "/", 999, 1, false, null));
        assertEquals(3, registry.get(MicrometerDispatchObserver.METER_NAME).tags("status", "200").timer().count());
        assertEquals(1, registry.get(MicrometerDispatchObserver.METER_NAME).tags("status", "OTHER").timer().count());
        assertEquals("OTHER", MicrometerDispatchObserver.statusTag(42));
    }

    private static final class RecordingObserver implements DispatchObserver {
        final List<DispatchEvent> events = new ArrayList<>();

        @Override
        public void onComplete(DispatchEvent event) {
            events.add(event);
        }
    }

    private static VesperaProxyController controller(
            DispatchModeResolver modes, DispatchObserver... observers) {
        return new VesperaProxyController(
                new HeaderAppNameResolver("X-Vespera-App", List.of("public")),
                modes, Runnable::run, true, 0, 0, "X-Request-Id", List.of(observers));
    }

    @Test
    void unknownAppIsAPlainText404ObservedWithTheRequestId() throws Exception {
        RecordingObserver observer = new RecordingObserver();
        DispatchObserver failing = event -> {
            throw new IllegalStateException("observer bug");
        };
        VesperaProxyController controller = controller(request -> {
            throw new AssertionError("mode must not be resolved");
        }, failing, observer);
        MockHttpServletRequest request = new MockHttpServletRequest("GET", "/users");
        request.addHeader("X-Vespera-App", "admin");
        request.addHeader("X-Request-Id", "req-42");
        MockHttpServletResponse response = new MockHttpServletResponse();

        assertNull(controller.proxy(request, response));

        assertEquals(404, response.getStatus());
        assertEquals("req-42", response.getHeader("X-Request-Id"));
        assertEquals(VesperaProxyController.UNKNOWN_APP_MSG, response.getContentAsString());
        assertEquals(1, observer.events.size(), "a failing observer does not stop the others");
        DispatchEvent event = observer.events.get(0);
        assertEquals(AppTags.UNKNOWN, event.app(), "the rejected name is never reported");
        assertNull(event.mode());
        assertEquals("GET", event.method());
        assertEquals(404, event.status());
        assertFalse(event.panicked());
        assertEquals("req-42", event.requestId());
        assertTrue(event.durationNanos() >= 0);
        assertNull(MDC.get(VesperaProxyController.MDC_REQUEST_ID), "MDC is cleared");
    }

    @Test
    void unexpectedFailuresBecomeA500AndCommittedOnesAreRethrown() throws Exception {
        AtomicReference<String> mdcSeen = new AtomicReference<>();
        RecordingObserver observer = new RecordingObserver();
        VesperaProxyController controller = controller(request -> {
            mdcSeen.set(MDC.get(VesperaProxyController.MDC_REQUEST_ID));
            throw new IllegalStateException("resolver exploded");
        }, observer);
        MockHttpServletResponse response = new MockHttpServletResponse();

        assertNull(controller.proxy(new MockHttpServletRequest("GET", "/x"), response));

        assertEquals(500, response.getStatus());
        assertEquals("vespera dispatch failed", response.getContentAsString(),
                "the exception message stays in the log");
        String generated = response.getHeader("X-Request-Id");
        assertEquals(generated, mdcSeen.get(), "the id is in the MDC while dispatching");
        assertEquals(500, observer.events.get(0).status());

        IllegalStateException committedFailure = new IllegalStateException("late");
        VesperaProxyController late = controller(request -> {
            throw committedFailure;
        }, observer);
        MockHttpServletResponse committed = new MockHttpServletResponse();
        committed.setCommitted(true);
        assertSame(committedFailure, assertThrows(IllegalStateException.class,
                () -> late.proxy(new MockHttpServletRequest("GET", "/x"), committed)));
        assertEquals(2, observer.events.size());
    }

    @Test
    void exchangeCompletesOnceAndLogsServerErrors() {
        RecordingObserver observer = new RecordingObserver();
        ProxyExchange exchange = new ProxyExchange(
                LoggerFactory.getLogger(ProxyOperabilityTest.class), List.of(observer), "PUT", null);
        exchange.app = "public";
        exchange.mode = DispatchMode.SYNC;
        exchange.path = "/p";
        exchange.reachedRust = true;
        exchange.complete(500, true);
        exchange.complete(200, false);
        assertEquals(1, observer.events.size());
        DispatchEvent event = observer.events.get(0);
        assertEquals("public", event.app());
        assertEquals(DispatchMode.SYNC, event.mode());
        assertEquals("/p", event.path());
        assertTrue(event.panicked());
        assertNull(event.requestId());

        ProxyExchange silent = new ProxyExchange(
                LoggerFactory.getLogger(ProxyOperabilityTest.class), List.of(), "GET", "r");
        silent.complete(503, false);
        silent.complete(200, false);
        assertTrue(silent.isCompleted());
    }

    @Test
    void exchangeCompletionRunsWithTheRequestIdInTheMdc() {
        AtomicReference<String> seen = new AtomicReference<>();
        ProxyExchange exchange = new ProxyExchange(LoggerFactory.getLogger(ProxyOperabilityTest.class),
                List.of(event -> seen.set(MDC.get(VesperaProxyController.MDC_REQUEST_ID))), "GET", "req-9");
        MDC.put(VesperaProxyController.MDC_REQUEST_ID, "outer");
        try {
            exchange.complete(200, false);
            assertEquals("req-9", seen.get());
            assertEquals("outer", MDC.get(VesperaProxyController.MDC_REQUEST_ID), "previous value restored");
        } finally {
            MDC.remove(VesperaProxyController.MDC_REQUEST_ID);
        }
        assertEquals("x", ProxyExchange.withMdc(null, () -> "x"));
        assertNull(MDC.get(VesperaProxyController.MDC_REQUEST_ID));
    }

    @Test
    void nullObserverListAndBlankHeaderDisableTheFeatures() throws Exception {
        VesperaProxyController controller = new VesperaProxyController(
                request -> {
                    throw new UnknownAppException("x");
                },
                request -> DispatchMode.SYNC, Runnable::run, true, 0, 0, "  ", null);
        MockHttpServletResponse response = new MockHttpServletResponse();
        assertNull(controller.proxy(new MockHttpServletRequest("GET", "/"), response));
        assertEquals(404, response.getStatus());
        assertFalse(response.containsHeader("X-Request-Id"));
    }
}
