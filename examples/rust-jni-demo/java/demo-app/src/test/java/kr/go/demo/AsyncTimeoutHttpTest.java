package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;

import com.devfive.vespera.bridge.DispatchEvent;
import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import com.devfive.vespera.bridge.DispatchObserver;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.system.CapturedOutput;
import org.springframework.boot.test.system.OutputCaptureExtension;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;

/**
 * An {@code ASYNC} dispatch that outlives Spring MVC's async request timeout
 * is answered {@code 503}, reported to observers exactly once, and the native
 * dispatch is cancelled: the Rust handler is dropped (never finishes) and its
 * in-flight slot is released, without errors in the log.
 */
@ExtendWith(OutputCaptureExtension.class)
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, AsyncTimeoutHttpTest.AsyncConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "spring.mvc.async.request-timeout=" + AsyncTimeoutHttpTest.TIMEOUT_MILLIS + "ms")
class AsyncTimeoutHttpTest extends AbstractVesperaHttpIntegrationTest {

    static final long TIMEOUT_MILLIS = 300;
    private static final long HANDLER_MILLIS = 1500;
    /** The in-flight count while only the diagnostic probe itself is in flight. */
    private static final long PROBE_SLOT = 1;

    @TestConfiguration(proxyBeanMethods = false)
    static class AsyncConfiguration {
        @Bean
        DispatchModeResolver asyncDispatchModeResolver() {
            return request -> DispatchMode.ASYNC;
        }

        @Bean
        RecordingObserver timeoutObserver() {
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

    private HttpResponse<String> get(String path, String requestId) throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + path))
                .header("X-Request-Id", requestId)
                .GET()
                .build();
        return client.send(request, HttpResponse.BodyHandlers.ofString());
    }

    /**
     * The process-wide count of ASYNC dispatches holding an in-flight slot.
     * The probe is itself an ASYNC dispatch in this context, so its own slot
     * is always counted while it reads the value.
     */
    private long asyncInFlight(String requestId) throws Exception {
        // The probe is ASYNC too, so on a slow run it can outlive the request timeout and get
        // the timeout's 503 (not a number): retry until a 200 with the count comes back.
        long deadline = System.nanoTime() + 10_000_000_000L;
        while (true) {
            HttpResponse<String> response = get("/diag/async-in-flight", requestId);
            if (response.statusCode() == 200) {
                return Long.parseLong(response.body());
            }
            if (System.nanoTime() > deadline) {
                throw new AssertionError("the in-flight probe kept failing: " + response.statusCode()
                        + " " + response.body());
            }
            Thread.sleep(10);
        }
    }

    /**
     * Waits until the probe's own slot is the only one held (a cancelled
     * dispatch from an earlier request may release its slot late), and returns
     * that baseline.
     */
    private long awaitAsyncIdle(String requestId) throws Exception {
        long deadline = System.nanoTime() + 10_000_000_000L;
        long inFlight = asyncInFlight(requestId);
        while (inFlight != PROBE_SLOT) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError("async dispatches still in flight: " + inFlight);
            }
            Thread.sleep(10);
            inFlight = asyncInFlight(requestId);
        }
        return PROBE_SLOT;
    }

    private List<DispatchEvent> eventsFor(String requestId) {
        return observer.events.stream().filter(e -> requestId.equals(e.requestId())).toList();
    }

    @Test
    void asyncTimeoutIsA503RecordedOnceAndCancelsTheHandler(CapturedOutput output) throws Exception {
        String key = "async-timeout-" + System.nanoTime();
        String requestId = key;

        // The in-flight counter is process-wide: wait until only the probe is
        // in flight, take that as the baseline, and wait for the slot to return
        // to it after the cancel.
        long baseline = awaitAsyncIdle(key + "-baseline");
        HttpResponse<String> response = get("/diag/sleep/" + key + "/" + HANDLER_MILLIS, requestId);
        assertEquals(503, response.statusCode(), response.body());

        // The timeout cancelled the native future: the sleeping handler is
        // dropped long before its HANDLER_MILLIS are over.
        long deadline = System.nanoTime() + 10_000_000_000L;
        while (!"1".equals(get("/diag/runs/" + key + ":dropped", key + "-poll").body())) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError("the timed-out handler was not cancelled");
            }
            Thread.sleep(50);
        }
        // The cancelled dispatch releases its in-flight slot once a Tokio
        // worker drops the task, which can lag the handler's drop probe.
        long slotDeadline = System.nanoTime() + 10_000_000_000L;
        long inFlight = asyncInFlight(key + "-slot");
        while (inFlight != baseline) {
            if (System.nanoTime() > slotDeadline) {
                throw new AssertionError("the in-flight slot was not released: " + inFlight
                        + " in flight, expected " + baseline);
            }
            Thread.sleep(10);
            inFlight = asyncInFlight(key + "-slot");
        }
        // Outlive the original handler delay: it must never finish.
        Thread.sleep(HANDLER_MILLIS);
        assertEquals("0", get("/diag/runs/" + key, key + "-poll2").body());

        List<DispatchEvent> events = eventsFor(requestId);
        assertEquals(1, events.size(), "reported exactly once: " + events);
        assertEquals(503, events.get(0).status());
        assertEquals(DispatchMode.ASYNC, events.get(0).mode());
        String log = output.getAll();
        assertFalse(log.contains(" ERROR "), log);
        assertFalse(log.contains("IllegalStateException"), log);
    }
}
