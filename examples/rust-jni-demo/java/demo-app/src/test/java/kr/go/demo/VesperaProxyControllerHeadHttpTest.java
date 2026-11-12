package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.annotation.Bean;

/**
 * HEAD through the real JNI proxy, in every dispatch mode.
 *
 * <p>axum answers {@code HEAD} by running the GET handler, keeping its
 * {@code content-length} and emptying the body. The proxy must forward that
 * representation length — not {@code Content-Length: 0} for the empty wire
 * body — and must not put any body bytes on the connection. A raw socket is
 * used so the assertion sees exactly what the servlet container framed.
 */
@SpringBootTest(
        classes = {
            VesperaHttpTestApplication.class,
            VesperaProxyControllerHeadHttpTest.PerRequestModeConfiguration.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT)
class VesperaProxyControllerHeadHttpTest extends AbstractVesperaHttpIntegrationTest {

    static final String MODE_HEADER = "X-Test-Dispatch-Mode";

    @TestConfiguration(proxyBeanMethods = false)
    static class PerRequestModeConfiguration {
        @Bean
        DispatchModeResolver perRequestDispatchModeResolver() {
            return request -> DispatchMode.valueOf(request.getHeader(MODE_HEADER));
        }
    }

    @ParameterizedTest
    @EnumSource(DispatchMode.class)
    void headAdvertisesGetContentLengthWithEmptyBody(DispatchMode mode) throws IOException {
        RawResponse get = raw("GET", "/health", mode);
        assertTrue(get.statusLine.contains(" 200"), get.statusLine);
        assertEquals("ok", new String(get.body, StandardCharsets.ISO_8859_1));

        RawResponse head = raw("HEAD", "/health", mode);

        assertTrue(head.statusLine.contains(" 200"), head.statusLine);
        assertEquals(String.valueOf(get.body.length), head.header("content-length"),
                mode + ": HEAD must advertise the GET body length\n" + head.headers);
        assertEquals(0, head.body.length, mode + ": HEAD must not carry body bytes");
    }

    private RawResponse raw(String method, String path, DispatchMode mode) throws IOException {
        URI root = URI.create(rest.getRootUri());
        try (Socket socket = new Socket(root.getHost(), root.getPort())) {
            socket.setSoTimeout(10_000);
            OutputStream out = socket.getOutputStream();
            out.write((method + " " + path + " HTTP/1.1\r\n"
                    + "Host: " + root.getHost() + "\r\n"
                    + MODE_HEADER + ": " + mode.name() + "\r\n"
                    + "Connection: close\r\n\r\n").getBytes(StandardCharsets.ISO_8859_1));
            out.flush();
            return RawResponse.parse(socket.getInputStream(), "HEAD".equals(method));
        }
    }

    private record RawResponse(String statusLine, String headers, byte[] body) {
        String header(String name) {
            String prefix = name.toLowerCase(Locale.ROOT) + ":";
            for (String line : headers.split("\r\n")) {
                if (line.toLowerCase(Locale.ROOT).startsWith(prefix)) {
                    return line.substring(prefix.length()).trim();
                }
            }
            return null;
        }

        static RawResponse parse(InputStream in, boolean head) throws IOException {
            byte[] all = in.readAllBytes();
            String text = new String(all, StandardCharsets.ISO_8859_1);
            int end = text.indexOf("\r\n\r\n");
            assertTrue(end > 0, "incomplete response: " + text);
            String head0 = text.substring(0, end);
            int firstLineEnd = head0.indexOf("\r\n");
            String statusLine = firstLineEnd < 0 ? head0 : head0.substring(0, firstLineEnd);
            String headers = firstLineEnd < 0 ? "" : head0.substring(firstLineEnd + 2);
            byte[] rest = java.util.Arrays.copyOfRange(all, end + 4, all.length);
            RawResponse parsed = new RawResponse(statusLine, headers, rest);
            if (!head && "chunked".equalsIgnoreCase(parsed.header("transfer-encoding"))) {
                return new RawResponse(statusLine, headers, dechunk(rest));
            }
            return parsed;
        }

        private static byte[] dechunk(byte[] chunked) {
            ByteArrayOutputStream body = new ByteArrayOutputStream();
            String s = new String(chunked, StandardCharsets.ISO_8859_1);
            int pos = 0;
            while (true) {
                int lineEnd = s.indexOf("\r\n", pos);
                int size = Integer.parseInt(s.substring(pos, lineEnd).split(";")[0].trim(), 16);
                if (size == 0) {
                    return body.toByteArray();
                }
                body.write(chunked, lineEnd + 2, size);
                pos = lineEnd + 2 + size + 2;
            }
        }
    }
}
