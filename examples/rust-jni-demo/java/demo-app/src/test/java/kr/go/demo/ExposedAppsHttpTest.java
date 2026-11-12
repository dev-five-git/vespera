package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

/**
 * {@code vespera.bridge.exposed-apps} restricts the client-controlled
 * {@code X-Vespera-App} header: listed apps (and the default app) stay
 * reachable, any other app is a {@code 404} from the proxy itself — the
 * request never reaches Rust (whose own unknown-app 404 names the app).
 */
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.exposed-apps=admin")
class ExposedAppsHttpTest extends AbstractVesperaHttpIntegrationTest {

    private ResponseEntity<byte[]> getWithApp(String path, String app) {
        HttpHeaders headers = new HttpHeaders();
        headers.set("X-Vespera-App", app);
        return exchangeWithHeaders(HttpMethod.GET, path, headers, null);
    }

    @Test
    void listedAndDefaultAppsStayReachable() {
        assertEquals(HttpStatus.OK, getWithApp("/dashboard", "admin").getStatusCode());
        assertEquals(HttpStatus.OK, getWithApp("/health", "_default").getStatusCode());
        assertEquals(HttpStatus.OK, exchange(HttpMethod.GET, "/health", null, null).getStatusCode());
    }

    @Test
    void unlistedAppIsRejectedByTheProxyWithoutDispatch() {
        ResponseEntity<byte[]> response = getWithApp("/health", "missing");

        assertEquals(HttpStatus.NOT_FOUND, response.getStatusCode());
        String body = response.getBody() == null
                ? ""
                : new String(response.getBody(), StandardCharsets.UTF_8);
        // Rust's own unknown-app 404 names the app; the proxy's does not.
        assertFalse(body.contains("no app"), body);
        assertFalse(body.contains("missing"), body);
    }
}
