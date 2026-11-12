package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.BufferedReader;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

/**
 * Streaming robustness through Tomcat with the default smart resolver:
 * server-sent events arrive incrementally (flushed per event), and a DIRECT
 * response larger than the pooled buffer is served from the overflow stash
 * without re-running the handler.
 */
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.dispatch-mode=smart")
class StreamingRobustnessHttpTest extends AbstractVesperaHttpIntegrationTest {

    @LocalServerPort
    private int port;

    /** Arrival time (ms since the request was sent) of each {@code data:} line. */
    private List<Long> eventArrivals(String accept) throws Exception {
        HttpRequest.Builder request = HttpRequest.newBuilder(
                        URI.create("http://localhost:" + port + "/stream/events"))
                .timeout(Duration.ofSeconds(30));
        if (accept != null) {
            request.header("Accept", accept);
        }
        long started = System.nanoTime();
        HttpResponse<InputStream> response = HttpClient.newHttpClient()
                .send(request.build(), HttpResponse.BodyHandlers.ofInputStream());
        assertEquals(200, response.statusCode());
        assertTrue(response.headers().firstValue("content-type").orElse("")
                .startsWith("text/event-stream"));
        List<Long> arrivals = new ArrayList<>();
        List<String> events = new ArrayList<>();
        try (BufferedReader reader = new BufferedReader(
                new InputStreamReader(response.body(), StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                if (line.startsWith("data:")) {
                    arrivals.add((System.nanoTime() - started) / 1_000_000);
                    events.add(line);
                }
            }
        }
        assertEquals(List.of("data: event-0", "data: event-1", "data: event-2"), events);
        return arrivals;
    }

    private static void assertIncremental(List<Long> arrivals) {
        // Events are produced 300 ms apart: the first must reach the client
        // well before the last one is produced (~600 ms), not all at the end.
        assertTrue(arrivals.get(2) - arrivals.get(0) >= 400,
                "events arrived together (buffered): " + arrivals);
    }

    @Test
    void eventStreamAcceptIsStreamedIncrementally() throws Exception {
        assertIncremental(eventArrivals("text/event-stream"));
    }

    @Test
    void eventStreamReachingDirectIsHandedOverForStreaming() throws Exception {
        // No streaming Accept: smart routes the bodyless GET to DIRECT; the
        // native side sees text/event-stream and hands it over for streaming.
        assertIncremental(eventArrivals(null));
    }

    private int[] runs() {
        ResponseEntity<byte[]> count = exchange(HttpMethod.GET, "/stream/count", null, null);
        String[] parts = new String(requireBody(count), StandardCharsets.UTF_8).split(",");
        return new int[] {Integer.parseInt(parts[0]), Integer.parseInt(parts[1])};
    }

    @Test
    void directOverflowRunsTheHandlerOnce() {
        int[] before = runs();
        ResponseEntity<byte[]> big = exchange(HttpMethod.GET, "/stream/big", null, null);
        assertEquals(HttpStatus.OK, big.getStatusCode());
        assertEquals(200 * 1024, requireBody(big).length);
        ResponseEntity<byte[]> huge = exchange(HttpMethod.GET, "/stream/huge", null, null);
        assertEquals(HttpStatus.OK, huge.getStatusCode());
        assertEquals(5 * 1024 * 1024, requireBody(huge).length);
        int[] after = runs();
        assertEquals(before[0] + 1, after[0], "200 KiB handler ran once");
        assertEquals(before[1] + 1, after[1], "5 MiB handler ran once");
    }

    private static byte[] requireBody(ResponseEntity<byte[]> response) {
        byte[] body = response.getBody();
        return body == null ? new byte[0] : body;
    }
}
