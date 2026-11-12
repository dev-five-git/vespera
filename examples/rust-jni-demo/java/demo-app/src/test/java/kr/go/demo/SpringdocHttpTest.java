package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

/**
 * springdoc integration ({@code vespera.bridge.springdoc.mode=group}, the
 * default) through the real cdylib: each exposed Rust app's
 * {@code vespera!(spec_url = "/openapi.json")} document is fetched by an
 * in-process native dispatch and listed in Swagger UI next to the Java one.
 */
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.exposed-apps=admin")
class SpringdocHttpTest extends AbstractVesperaHttpIntegrationTest {

    private String get(String path) {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, path, null, null);
        assertEquals(HttpStatus.OK, response.getStatusCode(), path);
        return new String(response.getBody(), StandardCharsets.UTF_8);
    }

    @Test
    void rustDocumentsAreServedPerApp() {
        String main = get("/v3/api-docs/vespera/_default");
        assertTrue(main.contains("Document Validation API"), main);
        assertTrue(main.contains("\"/health\""), main);

        String admin = get("/v3/api-docs/vespera/admin");
        assertTrue(admin.contains("Admin Dashboard API"), admin);
        assertTrue(admin.contains("\"/dashboard\""), admin);

        assertEquals(HttpStatus.NOT_FOUND,
                exchange(HttpMethod.GET, "/v3/api-docs/vespera/missing", null, null).getStatusCode());
    }

    @Test
    void swaggerUiListsTheJavaAndRustDocuments() {
        String config = get("/v3/api-docs/swagger-config");
        assertTrue(config.contains("\"url\":\"/v3/api-docs\""), config);
        assertTrue(config.contains("\"url\":\"/v3/api-docs/vespera/_default\""), config);
        assertTrue(config.contains("\"url\":\"/v3/api-docs/vespera/admin\""), config);

        // springdoc's own (Java) document does not list the proxy catch-all.
        String java = get("/v3/api-docs");
        assertFalse(java.contains("/**"), java);
    }
}
