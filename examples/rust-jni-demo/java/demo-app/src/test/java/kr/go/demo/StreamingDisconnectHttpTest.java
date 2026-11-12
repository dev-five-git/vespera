package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;

/**
 * A real HTTP client that disconnects mid-download: the proxy's next servlet
 * write fails, and the Rust response body is dropped instead of trickling on
 * for minutes. The {@code X-Mode} request header picks the streaming mode.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, StreamingDisconnectHttpTest.ModeConfiguration.class},
        webEnvironment = WebEnvironment.RANDOM_PORT)
class StreamingDisconnectHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class ModeConfiguration {
        @Bean
        DispatchModeResolver headerDispatchModeResolver() {
            return request -> {
                String mode = request.getHeader("X-Mode");
                return mode == null ? DispatchMode.SYNC : DispatchMode.valueOf(mode);
            };
        }
    }

    @LocalServerPort
    private int port;

    private String body(String path) {
        return new String(rest.getForObject(path, byte[].class), StandardCharsets.UTF_8);
    }

    @ParameterizedTest
    @EnumSource(value = DispatchMode.class, names = {"STREAMING", "BIDIRECTIONAL_STREAMING"})
    void clientDisconnectDropsTheRustBody(DispatchMode mode) throws Exception {
        String key = "http-disconnect-" + mode + "-" + System.nanoTime();
        try (Socket socket = new Socket("localhost", port)) {
            OutputStream out = socket.getOutputStream();
            out.write(("GET /diag/drip/" + key + "/10000/20 HTTP/1.1\r\n"
                    + "Host: localhost\r\nX-Mode: " + mode + "\r\n\r\n")
                    .getBytes(StandardCharsets.US_ASCII));
            out.flush();
            // Read the status line and some body, proving the stream started.
            InputStream in = socket.getInputStream();
            byte[] buf = new byte[4096];
            int read = in.read(buf);
            assertTrue(read > 0);
            assertTrue(new String(buf, 0, read, StandardCharsets.US_ASCII).startsWith("HTTP/1.1 200"));
            socket.setSoLinger(true, 0); // close with RST, like a killed browser tab
        }

        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(15);
        while (!"1".equals(body("/diag/runs/" + key + ":dropped"))) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError("the Rust body was not dropped after the disconnect");
            }
            Thread.sleep(50);
        }
        assertEquals("0", body("/diag/runs/" + key), "the drip never ran to its end");
    }
}
