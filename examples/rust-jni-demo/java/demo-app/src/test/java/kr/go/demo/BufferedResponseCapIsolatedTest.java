package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;

/**
 * {@code vespera.bridge.max-buffered-response-bytes} is forwarded to the
 * native buffered-response cap, and an oversized {@code SYNC} / {@code ASYNC}
 * response is answered {@code 500} by Rust before the body is buffered.
 *
 * <p>The native cap is fixed for the whole process on the first dispatch, so
 * this class is tagged {@value #TAG} and runs in its own JVM (the
 * {@code isolatedJvmTest} Gradle task, finalizing {@code test}); the main
 * suite excludes the tag and keeps the default cap.
 */
@Tag(BufferedResponseCapIsolatedTest.TAG)
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, BufferedResponseCapIsolatedTest.ModeConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.max-buffered-response-bytes=" + BufferedResponseCapIsolatedTest.CAP)
class BufferedResponseCapIsolatedTest extends AbstractVesperaHttpIntegrationTest {

    static final String TAG = "isolated-jvm";
    static final int CAP = 65536;
    private static final String MODE_HEADER = "X-Test-Dispatch-Mode";

    @TestConfiguration(proxyBeanMethods = false)
    static class ModeConfiguration {
        /** The dispatch mode named by {@value #MODE_HEADER}, else SYNC. */
        @Bean
        DispatchModeResolver headerDispatchModeResolver() {
            return request -> {
                String mode = request.getHeader(MODE_HEADER);
                return mode == null ? DispatchMode.SYNC : DispatchMode.valueOf(mode);
            };
        }
    }

    @LocalServerPort
    private int port;

    private final HttpClient client = HttpClient.newHttpClient();

    private HttpResponse<String> get(String path, DispatchMode mode) throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + path))
                .header(MODE_HEADER, mode.name())
                .GET()
                .build();
        return client.send(request, HttpResponse.BodyHandlers.ofString());
    }

    @ParameterizedTest
    @EnumSource(value = DispatchMode.class, names = {"SYNC", "ASYNC"})
    void oversizedBufferedResponseIsA500FromTheNativeCap(DispatchMode mode) throws Exception {
        String key = "cap-" + mode.name().toLowerCase() + "-" + System.nanoTime();

        HttpResponse<String> over = get("/diag/blob/" + key + "/" + (CAP * 3), mode);

        assertEquals(500, over.statusCode(), over.body());
        // Rust's message, not the Java fallback check: the cap reached native.
        assertTrue(over.body().contains("configured maximum buffered size of " + CAP), over.body());
        assertEquals("1", get("/diag/runs/" + key, DispatchMode.SYNC).body(),
                "the handler ran exactly once (no retry)");

        HttpResponse<String> under = get("/diag/blob/" + key + "-small/" + (CAP / 2), mode);
        assertEquals(200, under.statusCode());
        assertEquals(CAP / 2, under.body().length());
    }

    @ParameterizedTest
    @EnumSource(value = DispatchMode.class, names = {"STREAMING", "BIDIRECTIONAL_STREAMING"})
    void streamingModesAreNotCapped(DispatchMode mode) throws Exception {
        HttpResponse<String> response = get("/diag/blob/cap-stream-" + System.nanoTime() + "/" + (CAP * 3), mode);

        assertEquals(200, response.statusCode());
        assertEquals(CAP * 3, response.body().length());
    }
}
