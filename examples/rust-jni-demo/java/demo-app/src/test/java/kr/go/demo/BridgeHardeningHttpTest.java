package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.DispatchEvent;
import com.devfive.vespera.bridge.DispatchObserver;
import com.devfive.vespera.bridge.VesperaBridge;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;

/**
 * Bridge hardening end to end: CORS preflights reach the Rust router
 * ({@code vespera.bridge.cors=rust}, the default), client-selected app names
 * are reported bounded, and the Actuator {@code vespera} health indicator
 * reports the verified native version.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, BridgeHardeningHttpTest.ObserverConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = {
            "management.endpoints.web.exposure.include=health",
            "management.endpoint.health.show-details=always"
        })
class BridgeHardeningHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration
    static class ObserverConfiguration {
        @Bean
        RecordingObserver hardeningObserver() {
            return new RecordingObserver();
        }
    }

    static final class RecordingObserver implements DispatchObserver {
        final List<DispatchEvent> events = new CopyOnWriteArrayList<>();

        @Override
        public void onComplete(DispatchEvent event) {
            events.add(event);
        }
    }

    @LocalServerPort
    private int port;

    @Autowired
    private RecordingObserver observer;

    private final HttpClient client = HttpClient.newHttpClient();

    private HttpResponse<String> send(String method, String path, String... headers) throws Exception {
        HttpRequest.Builder builder = HttpRequest.newBuilder(URI.create("http://localhost:" + port + path))
                .method(method, HttpRequest.BodyPublishers.noBody());
        if (headers.length > 0) {
            builder.headers(headers);
        }
        return client.send(builder.build(), HttpResponse.BodyHandlers.ofString());
    }

    private DispatchEvent eventFor(String requestId) {
        assertNotNull(requestId, "response has no request id");
        long deadline = System.nanoTime() + 5_000_000_000L;
        do {
            for (DispatchEvent event : observer.events) {
                if (requestId.equals(event.requestId())) {
                    return event;
                }
            }
            Thread.onSpinWait();
        } while (System.nanoTime() < deadline);
        throw new AssertionError("no DispatchEvent recorded for request id " + requestId);
    }

    @Test
    void corsPreflightIsAnsweredByTheRustRouter() throws Exception {
        HttpResponse<String> response = send("OPTIONS", "/health",
                "Origin", "https://app.example",
                "Access-Control-Request-Method", "GET",
                "X-Request-Id", "preflight-1");

        // Spring's own preflight handling answers without dispatching; here the
        // proxy ran (request id echoed, observer event) and the Rust router
        // answered the OPTIONS itself (the demo router has no CORS layer).
        assertEquals("preflight-1", response.headers().firstValue("X-Request-Id").orElse(null));
        DispatchEvent event = eventFor("preflight-1");
        assertEquals("OPTIONS", event.method());
        assertEquals("/health", event.path());
        assertEquals(response.statusCode(), event.status());
    }

    @Test
    void clientSelectedAppNamesAreReportedBounded() throws Exception {
        HttpResponse<String> random = send("GET", "/health",
                "X-Vespera-App", "attacker-" + System.nanoTime(), "X-Request-Id", "app-random");
        assertEquals(404, random.statusCode(), "Rust: no app registered with that name");
        assertEquals("UNKNOWN", eventFor("app-random").app());

        HttpResponse<String> admin = send("GET", "/dashboard",
                "X-Vespera-App", "admin", "X-Request-Id", "app-admin");
        assertEquals(200, admin.statusCode());
        assertEquals("admin", eventFor("app-admin").app(), "a registered app, proven by its handler");

        HttpResponse<String> plain = send("GET", "/health", "X-Request-Id", "app-default");
        assertEquals(200, plain.statusCode());
        assertEquals("_default", eventFor("app-default").app());
    }

    @Test
    void healthIndicatorIsUpAndReportsMatchingVersions() throws Exception {
        HttpResponse<String> health = send("GET", "/actuator/health/vespera");

        assertEquals(200, health.statusCode(), health.body());
        String body = health.body();
        assertTrue(body.contains("\"status\":\"UP\""), body);
        assertTrue(VesperaBridge.isInitialized());
        // init() verified bridge and native versions share major.minor.
        assertTrue(body.contains("\"nativeVersion\":\""), body);
        assertTrue(body.contains("\"bridgeVersion\":\""), body);
    }
}
