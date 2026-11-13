package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.fail;

import com.devfive.vespera.bridge.DispatchEvent;
import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchObserver;
import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;

/**
 * A Rust handler panic on a request the default smart resolver routes
 * through {@link DispatchMode#BIDIRECTIONAL_STREAMING} must never reach the
 * client as a clean {@code 200}.  A panic before the response header answers
 * {@code 500} (panicked); a panic in the body stream after the header still
 * answers the proxy's own {@code 500} (panicked) while nothing reached the
 * client, and aborts the connection once bytes did — the client can always
 * tell the response is incomplete.
 *
 * <p>Both request-body paths of the bidirectional dispatch are covered: a
 * small chunked body (one chunk — pulled inline on the calling thread) and a
 * 1 MiB body (the spawned request producer).
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, BidirectionalPanicStatusHttpTest.ObserverConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.dispatch-mode=smart")
class BidirectionalPanicStatusHttpTest {

    static {
        VesperaBridge.init("rust_jni_demo");
    }

    /** Request body shapes the smart resolver streams bidirectionally. */
    enum Upload {
        /** Unknown length (chunked), fits one streaming chunk: the inline pull path. */
        SMALL_CHUNKED(4 * 1024, true),
        /** Known length past the SYNC/DIRECT caps, many chunks: the producer path. */
        LARGE(1024 * 1024, false);

        final int bytes;
        final boolean chunked;

        Upload(int bytes, boolean chunked) {
            this.bytes = bytes;
            this.chunked = chunked;
        }
    }

    @TestConfiguration
    static class ObserverConfiguration {
        @Bean
        RecordingObserver bidiPanicObserver() {
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

    private final HttpClient client = HttpClient.newBuilder()
            .version(HttpClient.Version.HTTP_1_1)
            .connectTimeout(Duration.ofSeconds(10))
            .build();

    @BeforeEach
    void clearEvents() {
        observer.events.clear();
    }

    private HttpResponse<InputStream> post(String path, Upload upload) throws Exception {
        byte[] body = new byte[upload.bytes];
        HttpRequest.BodyPublisher publisher = upload.chunked
                ? HttpRequest.BodyPublishers.ofInputStream(() -> new ByteArrayInputStream(body))
                : HttpRequest.BodyPublishers.ofByteArray(body);
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + path))
                .timeout(Duration.ofSeconds(30))
                .header("Content-Type", "application/octet-stream")
                .POST(publisher)
                .build();
        return client.send(request, HttpResponse.BodyHandlers.ofInputStream());
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

    private void assertPanicked500(String path, Upload upload) throws Exception {
        HttpResponse<InputStream> response = post(path, upload);
        byte[] body;
        try (InputStream in = response.body()) {
            body = in.readAllBytes();
        }
        String where = path + " " + upload;
        assertEquals(500, response.statusCode(), where);
        // A pre-header panic answers through the header callback (status +
        // headers only, empty body); a post-header one through the proxy's
        // own error body.  Never any of the handler's partial body.
        String text = new String(body, StandardCharsets.UTF_8);
        assertTrue(text.isEmpty() || text.equals("panic in Rust engine"), where + ": " + text);
        DispatchEvent event = eventFor(response.headers().firstValue("X-Request-Id").orElse(null));
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, event.mode(), event.toString());
        assertTrue(event.panicked(), event.toString());
        assertEquals(500, event.status(), event.toString());
    }

    /** Panic before the handler touched the request body. */
    @ParameterizedTest
    @EnumSource(Upload.class)
    void panicBeforeReadingTheBodyAnswers500(Upload upload) throws Exception {
        assertPanicked500("/echo/panic", upload);
    }

    /** Panic after the handler read the whole request body (inline pull / producer). */
    @ParameterizedTest
    @EnumSource(Upload.class)
    void panicAfterReadingTheBodyAnswers500(Upload upload) throws Exception {
        assertPanicked500("/diag/panic-after-read", upload);
    }

    /** The header was produced, the body stream panics on its first poll. */
    @ParameterizedTest
    @EnumSource(Upload.class)
    void panicInTheFirstBodyPollAnswers500(Upload upload) throws Exception {
        assertPanicked500("/diag/panic-mid-body/0", upload);
    }

    /** Body bytes still in the (uncommitted) servlet buffer are discarded for the 500. */
    @ParameterizedTest
    @EnumSource(Upload.class)
    void panicAfterBufferedBodyBytesAnswers500(Upload upload) throws Exception {
        assertPanicked500("/diag/panic-mid-body/2", upload);
    }

    /**
     * A body panic after bytes reached the client (512 KiB, well past the
     * servlet buffer) cannot change the committed {@code 200}: the connection
     * must be aborted so the client sees an incomplete response, never a
     * cleanly terminated short body.
     */
    @ParameterizedTest
    @EnumSource(Upload.class)
    void panicAfterTheBodyWasCommittedAbortsTheResponse(Upload upload) throws Exception {
        HttpResponse<InputStream> response;
        try {
            response = post("/diag/panic-mid-body/512", upload);
        } catch (IOException aborted) {
            return; // aborted before the status line arrived: also incomplete
        }
        assertEquals(200, response.statusCode(), "512 KiB exceed the servlet buffer");
        int received = 0;
        try (InputStream body = response.body()) {
            byte[] buffer = new byte[64 * 1024];
            for (int n; (n = body.read(buffer)) != -1; ) {
                received += n;
            }
        } catch (IOException aborted) {
            assertTrue(received < 512 * 1024, "received " + received);
            DispatchEvent event = eventFor(response.headers().firstValue("X-Request-Id").orElse(null));
            assertTrue(event.panicked(), event.toString());
            return;
        }
        fail("a 200 response truncated by a panic (" + received
                + " bytes) ended cleanly: the client cannot tell it is incomplete");
    }
}
