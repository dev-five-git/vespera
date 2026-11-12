package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.web.server.LocalServerPort;

/**
 * {@code vespera.bridge.cors=spring} keeps Spring MVC's own preflight
 * handling — the pre-0.6 behaviour, and the reason {@code rust} became the
 * default: without a Spring CORS configuration Spring answers the preflight
 * itself (no {@code Access-Control-Allow-Origin}, so the browser blocks the
 * real request) and it never reaches the Rust router (no request id is echoed,
 * because the proxy never ran).
 */
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.cors=spring")
class CorsSpringModeHttpTest extends AbstractVesperaHttpIntegrationTest {

    @LocalServerPort
    private int port;

    @Test
    void springAnswersThePreflightWithoutDispatching() throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + "/health"))
                .method("OPTIONS", HttpRequest.BodyPublishers.noBody())
                .header("Origin", "https://app.example")
                .header("Access-Control-Request-Method", "GET")
                .build();
        HttpResponse<String> response =
                HttpClient.newHttpClient().send(request, HttpResponse.BodyHandlers.ofString());

        assertTrue(response.headers().firstValue("Access-Control-Allow-Origin").isEmpty(), "no CORS config in Spring");
        assertTrue(response.headers().firstValue("X-Request-Id").isEmpty(), "the proxy never ran");
    }
}
