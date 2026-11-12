package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * {@code vespera.bridge.exclude-patterns}: excluded paths are not mapped to
 * the proxy, so Spring MVC serves them (static resources from
 * {@code classpath:/static/}, other controllers, else its own {@code 404});
 * every other path is still proxied to the real Rust router.  The proxy echoes
 * {@code X-Request-Id} on everything it serves, so its absence proves the
 * proxy never ran.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, ProxyPathPatternsHttpTest.SpringOnlyController.class},
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.exclude-patterns=/assets/**,/spring/**")
class ProxyPathPatternsHttpTest extends AbstractVesperaHttpIntegrationTest {

    @RestController
    static class SpringOnlyController {
        @GetMapping("/spring/{name}")
        public String hello(@org.springframework.web.bind.annotation.PathVariable String name) {
            return "spring:" + name;
        }
    }

    private ResponseEntity<byte[]> get(String path) {
        return exchange(HttpMethod.GET, path, null, null);
    }

    private static String body(ResponseEntity<byte[]> response) {
        return response.getBody() == null ? "" : new String(response.getBody(), StandardCharsets.UTF_8);
    }

    @Test
    void excludedPathIsServedBySpringStaticResources() {
        ResponseEntity<byte[]> response = get("/assets/hello.txt");
        assertEquals(200, response.getStatusCode().value());
        assertEquals("served by Spring static resources", body(response));
        assertNull(response.getHeaders().getFirst("X-Request-Id"), "the proxy never ran");
    }

    @Test
    void excludedPathWithoutAHandlerIsSpringsOwn404() {
        ResponseEntity<byte[]> response = get("/assets/missing.txt");
        assertEquals(404, response.getStatusCode().value());
        assertNull(response.getHeaders().getFirst("X-Request-Id"), "the proxy never ran");
    }

    @Test
    void excludedPathIsServedByAnotherController() {
        ResponseEntity<byte[]> response = get("/spring/hi");
        assertEquals(200, response.getStatusCode().value());
        assertEquals("spring:hi", body(response));
        assertNull(response.getHeaders().getFirst("X-Request-Id"));
    }

    @Test
    void includedPathIsStillProxiedToRust() {
        ResponseEntity<byte[]> response = get("/health");
        assertEquals(200, response.getStatusCode().value());
        assertEquals("ok", body(response));
        assertNotNull(response.getHeaders().getFirst("X-Request-Id"), "served by the proxy");

        ResponseEntity<byte[]> missing = get("/no-such-rust-route");
        assertEquals(404, missing.getStatusCode().value());
        assertNotNull(missing.getHeaders().getFirst("X-Request-Id"), "Rust's 404, through the proxy");
    }

    @Test
    void preflightOnAnExcludedPathIsNotRoutedToRust() {
        HttpHeaders headers = new HttpHeaders();
        headers.setOrigin("https://app.example");
        headers.setAccessControlRequestMethod(HttpMethod.GET);
        ResponseEntity<byte[]> response =
                exchangeWithHeaders(HttpMethod.OPTIONS, "/assets/hello.txt", headers, null);
        assertNull(response.getHeaders().getFirst("X-Request-Id"), "Spring handled the preflight");
    }
}
