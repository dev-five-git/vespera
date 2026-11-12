package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.DispatchEvent;
import com.devfive.vespera.bridge.DispatchObserver;
import com.devfive.vespera.bridge.VesperaBridge;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.resttestclient.TestRestTemplate;
import org.springframework.boot.resttestclient.autoconfigure.AutoConfigureTestRestTemplate;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.annotation.Bean;
import org.springframework.http.HttpEntity;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

/**
 * Operability end to end: {@code vespera.bridge.library-name} auto-init (or a
 * no-op when another test class already loaded the library), client address
 * and scheme forwarding, request ids, and {@link DispatchObserver} events for
 * normal and panicking dispatches.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, OperabilityHttpTest.ObserverConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.library-name=rust_jni_demo")
@AutoConfigureTestRestTemplate
class OperabilityHttpTest {

    @TestConfiguration
    static class ObserverConfiguration {
        @Bean
        RecordingObserver recordingObserver() {
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

    @Autowired
    private TestRestTemplate rest;

    @Autowired
    private RecordingObserver observer;

    @BeforeEach
    void clearEvents() {
        observer.events.clear();
    }

    private ResponseEntity<byte[]> send(HttpMethod method, String path, HttpHeaders headers, byte[] body) {
        return rest.exchange(path, method, new HttpEntity<>(body, headers), byte[].class);
    }

    private static String text(ResponseEntity<byte[]> response) {
        return response.getBody() == null ? "" : new String(response.getBody(), StandardCharsets.UTF_8);
    }

    /**
     * The event for {@code requestId}. The observer fires on the server thread
     * after the response is written, so the client can see the response first:
     * wait for the event instead of reading the list immediately.
     */
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

    private static String requestIdOf(ResponseEntity<byte[]> response) {
        return response.getHeaders().getFirst("X-Request-Id");
    }

    @Test
    void libraryNameLeavesTheLibraryInitialised() {
        assertTrue(VesperaBridge.isInitialized());
    }

    @Test
    void rustSeesTheServletClientAddressAndScheme() {
        ResponseEntity<byte[]> response = send(HttpMethod.GET, "/headers/client", new HttpHeaders(), null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        String body = text(response);
        assertTrue(body.matches("http (127\\.0\\.0\\.1|\\[[0-9a-f:]+\\]):\\d+"), body);
    }

    @Test
    void incomingRequestIdIsForwardedEchoedAndReported() {
        HttpHeaders headers = new HttpHeaders();
        headers.set("X-Request-Id", "e2e-req-1");
        ResponseEntity<byte[]> response = send(HttpMethod.GET, "/headers", headers, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals("e2e-req-1", response.getHeaders().getFirst("X-Request-Id"));
        assertTrue(text(response).contains("\"x-request-id\""), text(response));
        DispatchEvent event = eventFor("e2e-req-1");
        assertEquals(200, event.status());
        assertEquals("/headers", event.path());
        assertFalse(event.panicked());
    }

    @Test
    void missingRequestIdIsGenerated() {
        ResponseEntity<byte[]> response = send(HttpMethod.GET, "/health", new HttpHeaders(), null);

        String id = response.getHeaders().getFirst("X-Request-Id");
        assertNotNull(id);
        assertEquals(36, id.length(), id);
        assertEquals(id, eventFor(id).requestId());
    }

    @Test
    void rustPanicIsReportedAsPanicked() {
        HttpHeaders headers = new HttpHeaders();
        headers.setContentType(MediaType.APPLICATION_OCTET_STREAM);
        ResponseEntity<byte[]> response = send(HttpMethod.POST, "/echo/panic", headers, new byte[] {1});

        assertEquals(HttpStatus.INTERNAL_SERVER_ERROR, response.getStatusCode());
        DispatchEvent event = eventFor(requestIdOf(response));
        assertTrue(event.panicked(), event.toString());
        assertEquals(500, event.status());
        assertEquals("POST", event.method());
    }
}
