package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

/**
 * {@code vespera.bridge.springdoc.mode=merge} through the real cdylib: the
 * Rust paths land in springdoc's own document, the admin app's operations
 * carrying the {@code X-Vespera-App} header parameter.
 */
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = {"vespera.bridge.exposed-apps=admin", "vespera.bridge.springdoc.mode=merge"})
class SpringdocMergeHttpTest extends AbstractVesperaHttpIntegrationTest {

    private static final String APP_HEADER = "X-Vespera-App";

    private final ObjectMapper objectMapper = new ObjectMapper();

    @Test
    void rustPathsAreMergedIntoSpringdocsDocument() throws Exception {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, "/v3/api-docs", null, null);
        assertEquals(HttpStatus.OK, response.getStatusCode());
        String docs = new String(response.getBody(), StandardCharsets.UTF_8);
        JsonNode root = objectMapper.readTree(docs);
        JsonNode paths = root.get("paths");
        assertNotNull(paths, docs);
        assertNotNull(paths.get("/health"), docs);
        JsonNode dashboard = paths.get("/dashboard");
        assertNotNull(dashboard, docs);

        // The admin app's operation carries the app-selection header, required, pinned to "admin".
        JsonNode header = null;
        for (JsonNode parameter : dashboard.get("get").get("parameters")) {
            if (APP_HEADER.equals(parameter.path("name").asText())) {
                header = parameter;
            }
        }
        assertNotNull(header, docs);
        assertEquals("header", header.get("in").asText());
        assertTrue(header.get("required").asBoolean(), docs);
        assertEquals("admin", header.get("schema").get("enum").get(0).asText());
        assertEquals(1, header.get("schema").get("enum").size());

        // The default app's operation does not.
        for (JsonNode parameter : paths.get("/health").get("get").path("parameters")) {
            assertFalse(APP_HEADER.equals(parameter.path("name").asText()), docs);
        }
    }
}
