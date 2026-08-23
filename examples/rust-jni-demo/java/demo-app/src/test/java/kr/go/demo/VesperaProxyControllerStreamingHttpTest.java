package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.dispatch-mode=bidirectional-streaming")
class VesperaProxyControllerStreamingHttpTest extends AbstractVesperaHttpIntegrationTest {

    @Test
    void bodylessGetUsesResponseOnlyStreaming() {
        ResponseEntity<byte[]> response =
                exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals("ok", new String(response.getBody(), StandardCharsets.UTF_8));
        assertNotNull(response.getHeaders().getContentType());
        assertTrue(response.getHeaders().getContentType().isCompatibleWith(MediaType.TEXT_PLAIN));
    }

    @Test
    void bodyfulPostUsesBidirectionalStreaming() {
        // `/echo/stream` returns the request body AS the response body, so this
        // exchange is genuinely full-duplex. A blocking HTTP client does not
        // read the response until it has finished writing the request, so the
        // payload MUST stay under the socket buffers on both sides or the two
        // sides deadlock until the connection resets. Large-body coverage of
        // the same dispatch mode lives on `/echo` (buffered) instead.
        byte[] payload = patternedBytes(16 * 1024);

        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/echo/stream",
                MediaType.APPLICATION_OCTET_STREAM,
                payload);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertArrayEquals(payload, response.getBody());
    }
}
