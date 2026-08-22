package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.dispatch-mode=smart")
class VesperaProxyControllerSmartHttpTest extends AbstractVesperaHttpIntegrationTest {

    private static final byte[] VALID_DOCUMENT = ("{"
                    + "\"documentType\":\"regulation\","
                    + "\"title\":\"Data Protection Policy\","
                    + "\"content\":\"This regulation establishes the framework for handling personal data within the organisation.\","
                    + "\"author\":\"Kim Minjun\","
                    + "\"department\":\"Information Security\","
                    + "\"classification\":\"internal\","
                    + "\"effectiveDate\":\"2025-01-01\"}")
            .getBytes(StandardCharsets.UTF_8);

    @Autowired
    private ObjectMapper objectMapper;

    @Test
    void bodylessGetUsesDirectAndPropagatesTextResponse() {
        ResponseEntity<byte[]> response =
                exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals("ok", new String(requireBody(response), StandardCharsets.UTF_8));
        assertNotNull(response.getHeaders().getContentType());
        assertTrue(response.getHeaders().getContentType().isCompatibleWith(MediaType.TEXT_PLAIN));
        assertEquals(2, response.getHeaders().getContentLength());
    }

    @Test
    void smallPostUsesSyncAndPropagatesJsonResponse() throws Exception {
        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/documents/validate",
                MediaType.APPLICATION_JSON,
                VALID_DOCUMENT);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals(MediaType.APPLICATION_JSON, response.getHeaders().getContentType());
        JsonNode json = objectMapper.readTree(requireBody(response));
        assertTrue(json.path("valid").asBoolean());
        assertTrue(json.path("documentId").asText().startsWith("DOC-"));
        assertEquals(0, json.path("errors").size());
    }

    @Test
    void invalidJsonShapePropagatesAxum422Rejection() {
        byte[] missingFields = "{\"documentType\":\"memo\"}"
                .getBytes(StandardCharsets.UTF_8);

        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/documents/validate",
                MediaType.APPLICATION_JSON,
                missingFields);

        assertEquals(HttpStatus.UNPROCESSABLE_ENTITY, response.getStatusCode());
        assertNotNull(response.getHeaders().getContentType());
        assertTrue(response.getHeaders().getContentType().isCompatibleWith(MediaType.TEXT_PLAIN));
        String body = new String(requireBody(response), StandardCharsets.UTF_8);
        assertTrue(body.contains("missing field"), body);
        assertTrue(body.contains("title"), body);
    }

    @Test
    void largeBinaryPostUsesBidirectionalStreamingWithoutChangingBytes() {
        byte[] payload = patternedBytes(512 * 1024);

        // `/echo` buffers the request body before responding, so the exchange
        // stays request-then-response. `/echo/stream` echoes the body as it
        // arrives, and a full-duplex exchange that large deadlocks against a
        // blocking HTTP client (it will not read the response until it has
        // finished writing the request) — see the small-payload case in
        // VesperaProxyControllerStreamingHttpTest.
        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/echo",
                MediaType.APPLICATION_OCTET_STREAM,
                payload);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertArrayEquals(payload, requireBody(response));
    }

    @Test
    void echoPreservesBinaryContentTypeAndBody() {
        byte[] payload = patternedBytes(4096);

        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST, "/echo", MediaType.APPLICATION_OCTET_STREAM, payload);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals(MediaType.APPLICATION_OCTET_STREAM, response.getHeaders().getContentType());
        assertArrayEquals(payload, requireBody(response));
    }

    @Test
    void adminHeaderSelectsNamedRustApp() throws Exception {
        HttpHeaders headers = new HttpHeaders();
        headers.set("X-Vespera-App", "admin");

        ResponseEntity<byte[]> response =
                exchangeWithHeaders(HttpMethod.GET, "/dashboard", headers, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals(MediaType.APPLICATION_JSON, response.getHeaders().getContentType());
        JsonNode json = objectMapper.readTree(requireBody(response));
        assertEquals("rust-jni-demo", json.path("system").asText());
        assertEquals("admin", json.path("mode").asText());
        assertEquals(42, json.path("activeUsers").asInt());
    }

    @Test
    void unknownAppPropagatesRust404() {
        HttpHeaders headers = new HttpHeaders();
        headers.set("X-Vespera-App", "missing");

        ResponseEntity<byte[]> response =
                exchangeWithHeaders(HttpMethod.GET, "/health", headers, null);

        assertEquals(HttpStatus.NOT_FOUND, response.getStatusCode());
        assertTrue(new String(requireBody(response), StandardCharsets.UTF_8).contains("missing"));
    }

    @Test
    void invalidAppNamePropagatesRust400() {
        HttpHeaders headers = new HttpHeaders();
        headers.set("X-Vespera-App", "not valid!");

        ResponseEntity<byte[]> response =
                exchangeWithHeaders(HttpMethod.GET, "/health", headers, null);

        assertEquals(HttpStatus.BAD_REQUEST, response.getStatusCode());
        String body = new String(requireBody(response), StandardCharsets.UTF_8);
        assertTrue(body.toLowerCase().contains("invalid app"), body);
    }

    @Test
    void handlerPanicPropagates500WithNoHang() {
        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/echo/panic",
                MediaType.APPLICATION_OCTET_STREAM,
                new byte[] {1});

        assertEquals(HttpStatus.INTERNAL_SERVER_ERROR, response.getStatusCode());
        // The wire error response carries a plain-text reason, not an empty
        // body — the panic is reported, never silently swallowed.
        assertEquals(
                "panic in Rust engine",
                new String(requireBody(response), StandardCharsets.UTF_8));
    }

    @Test
    void unknownRoutePropagates404StatusAndEmptyBody() {
        ResponseEntity<byte[]> response =
                exchange(HttpMethod.GET, "/route-that-does-not-exist", null, null);

        assertEquals(HttpStatus.NOT_FOUND, response.getStatusCode());
        assertEquals(0, requireBody(response).length);
    }

    @Test
    void headSuppressesBodyButRetainsGetContentLength() {
        ResponseEntity<byte[]> response =
                exchange(HttpMethod.HEAD, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals(0, requireBody(response).length);
    }

    private static byte[] requireBody(ResponseEntity<byte[]> response) {
        return response.getBody() == null ? new byte[0] : response.getBody();
    }
}
