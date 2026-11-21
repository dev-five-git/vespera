package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.Test;
import org.springframework.http.HttpHeaders;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import reactor.core.Disposable;

/**
 * Client disconnects on the WebFlux proxy and the app tag they leave behind:
 * a request whose wire response reached the proxy keeps the app the client
 * selected; one cancelled before any response (buffered, still computing, or
 * queued) reports {@link AppTags#UNKNOWN} unless its app name is one the
 * tags know without a Rust answer (the default app or an allowlisted name,
 * reported as itself) (see {@link VesperaReactiveProxyHandlerTest}
 * for the rest of the handler's behaviour).
 */
class VesperaReactiveProxyHandlerCancelTest {

    private final FakeTransport transport = new FakeTransport();
    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();

    private VesperaReactiveProxyHandler handler() {
        return ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App", null), null, 1024, 1024,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST);
    }

    private static MockServerWebExchange sse(String path, String app) {
        return MockServerWebExchange.from(MockServerHttpRequest.get(path)
                .header(HttpHeaders.ACCEPT, "text/event-stream")
                .header("X-Vespera-App", app));
    }

    /** The event of the request to {@code path} (events arrive after the response hand-off). */
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

    /**
     * Rust answered the stream (its header is out) and the client then went
     * away mid-body: the native call was made, so the app the client selected
     * keeps its tag. Without this an SSE-only app would stay "unknown" forever.
     */
    @Test
    void streamCancelledAfterRustAnsweredKeepsTheAppTag() throws Exception {
        CountDownLatch hold = new CountDownLatch(1);
        transport.streamAfterHeader = hold;
        try {
            Disposable running = handler().handle(sse("/billing/feed", "billing")).subscribe();
            assertTrue(transport.headersSent.tryAcquire(5, TimeUnit.SECONDS));
            running.dispose(); // the client disconnects while the body is in flight
            DispatchEvent event = awaitEvent("/billing/feed");
            assertEquals(VesperaReactiveProxyHandler.CLIENT_CLOSED_REQUEST, event.status());
            assertEquals("billing", event.app(), "Rust was reached: the selected app is learned");
        } finally {
            hold.countDown();
        }
    }

    /**
     * A buffered request cancelled while Rust still computes: no wire response
     * arrived, so the client-selected name is not learned (the reported app is
     * {@code UNKNOWN}), even though the native call was made.
     */
    @Test
    void bufferedCancelledWhileRustRunsDoesNotLearnTheAppTag() throws Exception {
        transport.pending = new CompletableFuture<>();
        Disposable running = handler().handle(MockServerWebExchange.from(MockServerHttpRequest.get("/orders")
                .header(HttpHeaders.ACCEPT, "application/json")
                .header("X-Vespera-App", "orders"))).subscribe();
        assertEquals(1, transport.requests.size());
        running.dispose();
        DispatchEvent event = awaitEvent("/orders");
        assertEquals(VesperaReactiveProxyHandler.CLIENT_CLOSED_REQUEST, event.status());
        assertEquals(AppTags.UNKNOWN, event.app(), "no answer arrived: the name is not learned");
    }
}
