package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

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

    @Test
    void rustPathsAreMergedIntoSpringdocsDocument() {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, "/v3/api-docs", null, null);
        assertEquals(HttpStatus.OK, response.getStatusCode());
        String docs = new String(response.getBody(), StandardCharsets.UTF_8);
        assertTrue(docs.contains("\"/health\""), docs);
        assertTrue(docs.contains("\"/dashboard\""), docs);
        assertTrue(docs.contains("X-Vespera-App"), docs);
    }
}
