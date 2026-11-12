package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import jakarta.servlet.Filter;
import jakarta.servlet.http.HttpServletRequest;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.annotation.Bean;
import org.springframework.core.Ordered;

/**
 * End-to-end through Tomcat + the real JNI bridge:
 * <ul>
 *   <li>form-urlencoded bodies consumed before the proxy (Spring's
 *       {@code FormContentFilter} for PATCH, a {@code getParameter} call —
 *       like Spring Security's {@code CsrfFilter} — for POST) still reach the
 *       Rust {@code Form<T>} extractor;</li>
 *   <li>the {@code X-Vespera-App} selection header is not forwarded;</li>
 *   <li>non-ASCII header bytes round-trip unchanged in both directions.</li>
 * </ul>
 * Raw sockets keep the exact request/response bytes under the test's control.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, FormAndHeaderHttpTest.ParameterReadingFilter.class},
        webEnvironment = WebEnvironment.RANDOM_PORT)
class FormAndHeaderHttpTest extends AbstractVesperaHttpIntegrationTest {

    /** Reads a POST parameter before the proxy, as Spring Security's CsrfFilter does. */
    @TestConfiguration(proxyBeanMethods = false)
    static class ParameterReadingFilter {
        @Bean
        FilterRegistrationBean<Filter> parameterReadingFilter() {
            FilterRegistrationBean<Filter> registration = new FilterRegistrationBean<>((request, response, chain) -> {
                if ("POST".equals(((HttpServletRequest) request).getMethod())) {
                    request.getParameter("_csrf");
                }
                chain.doFilter(request, response);
            });
            registration.setOrder(Ordered.LOWEST_PRECEDENCE);
            return registration;
        }
    }

    private final ObjectMapper objectMapper = new ObjectMapper();

    @LocalServerPort
    private int port;

    @Test
    void patchFormConsumedByFormContentFilterReachesRust() throws Exception {
        assertProfileEcho("PATCH");
    }

    @Test
    void postFormConsumedByGetParameterReachesRust() throws Exception {
        assertProfileEcho("POST");
    }

    private void assertProfileEcho(String method) throws Exception {
        byte[] form = "name=Jane+Doe&count=3&note=caf%C3%A9".getBytes(StandardCharsets.US_ASCII);
        RawResponse response = send(method + " /form?trace=1 HTTP/1.1\r\n"
                + "Content-Type: application/x-www-form-urlencoded\r\n"
                + "Content-Length: " + form.length + "\r\n", form);

        assertEquals(200, response.status, response.text());
        JsonNode json = objectMapper.readTree(response.body);
        assertEquals("Jane Doe", json.path("name").asText());
        assertEquals(3, json.path("count").asInt());
        assertEquals("café", json.path("note").asText());
    }

    @Test
    void appSelectionHeaderIsNotForwardedToRust() throws Exception {
        RawResponse response = send("GET /headers HTTP/1.1\r\n"
                + "X-Vespera-App: _default\r\n"
                + "X-Probe: 1\r\n", new byte[0]);

        assertEquals(200, response.status, response.text());
        JsonNode names = objectMapper.readTree(response.body);
        assertTrue(names.toString().contains("x-probe"), names.toString());
        assertFalse(names.toString().contains("x-vespera-app"), names.toString());
    }

    @Test
    void nonAsciiHeaderBytesRoundTripThroughRust() throws Exception {
        byte[] name = "résumé.pdf".getBytes(StandardCharsets.UTF_8);
        ByteArrayOutputStream head = new ByteArrayOutputStream();
        head.write("GET /headers/raw HTTP/1.1\r\nX-Name: ".getBytes(StandardCharsets.US_ASCII));
        head.write(name);
        head.write("\r\n".getBytes(StandardCharsets.US_ASCII));

        RawResponse response = send(head.toByteArray(), new byte[0]);

        assertEquals(200, response.status, response.text());
        assertArrayEquals(name, response.body, "request header bytes reached Rust unchanged");
        ByteArrayOutputStream disposition = new ByteArrayOutputStream();
        disposition.write("attachment; filename=\"".getBytes(StandardCharsets.US_ASCII));
        disposition.write(name);
        disposition.write('"');
        assertArrayEquals(disposition.toByteArray(), response.headers.get("content-disposition"),
                "response header bytes reached the client unchanged");
    }

    // ── raw HTTP/1.1 client ──────────────────────────────────────────

    private RawResponse send(String head, byte[] body) throws IOException {
        return send(head.getBytes(StandardCharsets.US_ASCII), body);
    }

    private RawResponse send(byte[] head, byte[] body) throws IOException {
        try (Socket socket = new Socket("localhost", port)) {
            socket.setSoTimeout(30_000);
            OutputStream out = socket.getOutputStream();
            out.write(head);
            out.write(("Host: localhost:" + port + "\r\nConnection: close\r\n\r\n")
                    .getBytes(StandardCharsets.US_ASCII));
            out.write(body);
            out.flush();
            return RawResponse.parse(socket.getInputStream().readAllBytes());
        }
    }

    private static final class RawResponse {
        final int status;
        final Map<String, byte[]> headers = new LinkedHashMap<>();
        final byte[] body;

        private RawResponse(int status, byte[] body) {
            this.status = status;
            this.body = body;
        }

        String text() {
            return new String(body, StandardCharsets.UTF_8);
        }

        static RawResponse parse(byte[] raw) {
            int headEnd = indexOf(raw, "\r\n\r\n".getBytes(StandardCharsets.US_ASCII), 0);
            String statusLine = new String(raw, 0, indexOf(raw, new byte[] {'\r', '\n'}, 0),
                    StandardCharsets.US_ASCII);
            Map<String, byte[]> headers = new LinkedHashMap<>();
            int pos = indexOf(raw, new byte[] {'\r', '\n'}, 0) + 2;
            while (pos < headEnd) {
                int lineEnd = indexOf(raw, new byte[] {'\r', '\n'}, pos);
                int colon = indexOf(raw, new byte[] {':'}, pos);
                String headerName = new String(raw, pos, colon - pos, StandardCharsets.US_ASCII)
                        .toLowerCase(Locale.ROOT);
                int valueStart = colon + 1;
                while (raw[valueStart] == ' ') {
                    valueStart++;
                }
                headers.put(headerName, java.util.Arrays.copyOfRange(raw, valueStart, lineEnd));
                pos = lineEnd + 2;
            }
            byte[] body = java.util.Arrays.copyOfRange(raw, headEnd + 4, raw.length);
            byte[] te = headers.get("transfer-encoding");
            if (te != null && new String(te, StandardCharsets.US_ASCII).contains("chunked")) {
                body = dechunk(body);
            }
            RawResponse response = new RawResponse(Integer.parseInt(statusLine.split(" ")[1]), body);
            response.headers.putAll(headers);
            return response;
        }

        private static byte[] dechunk(byte[] chunked) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            int pos = 0;
            while (true) {
                int lineEnd = indexOf(chunked, new byte[] {'\r', '\n'}, pos);
                String sizeLine = new String(chunked, pos, lineEnd - pos, StandardCharsets.US_ASCII);
                int size = Integer.parseInt(sizeLine.split(";")[0].trim(), 16);
                if (size == 0) {
                    return out.toByteArray();
                }
                out.write(chunked, lineEnd + 2, size);
                pos = lineEnd + 2 + size + 2;
            }
        }

        private static int indexOf(byte[] haystack, byte[] needle, int from) {
            outer:
            for (int i = from; i <= haystack.length - needle.length; i++) {
                for (int j = 0; j < needle.length; j++) {
                    if (haystack[i + j] != needle[j]) {
                        continue outer;
                    }
                }
                return i;
            }
            throw new IllegalStateException("malformed HTTP response");
        }
    }
}
