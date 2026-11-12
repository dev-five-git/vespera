package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;

import jakarta.servlet.Filter;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import java.nio.charset.StandardCharsets;
import java.security.Principal;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.annotation.Bean;
import org.springframework.core.Ordered;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.ResponseEntity;

/**
 * End-to-end through Tomcat + the real JNI bridge: the authenticated servlet
 * principal reaches the Rust {@code Principal} extractor through the trusted
 * {@code principal} wire field, in both the DIRECT and the streaming dispatch
 * modes, while an anonymous request (even one carrying a forged
 * {@code X-Principal} header) is rejected {@code 401} by Rust.
 *
 * <p>The test filter stands in for container / Spring Security
 * authentication: it authenticates a request carrying {@code X-Test-User}
 * by wrapping it so {@code getUserPrincipal()} returns that user.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, PrincipalForwardingHttpTest.Authenticate.class},
        webEnvironment = WebEnvironment.RANDOM_PORT)
class PrincipalForwardingHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class Authenticate {
        @Bean
        FilterRegistrationBean<Filter> authenticateFilter() {
            FilterRegistrationBean<Filter> registration = new FilterRegistrationBean<>((request, response, chain) -> {
                HttpServletRequest http = (HttpServletRequest) request;
                String user = http.getHeader("X-Test-User");
                if (user == null) {
                    chain.doFilter(request, response);
                    return;
                }
                Principal principal = () -> user;
                chain.doFilter(new HttpServletRequestWrapper(http) {
                    @Override
                    public Principal getUserPrincipal() {
                        return principal;
                    }
                }, response);
            });
            registration.setOrder(Ordered.HIGHEST_PRECEDENCE);
            return registration;
        }
    }

    private ResponseEntity<byte[]> get(String user, String accept, String forged) {
        HttpHeaders headers = new HttpHeaders();
        if (user != null) {
            headers.set("X-Test-User", user);
        }
        if (accept != null) {
            headers.set(HttpHeaders.ACCEPT, accept);
        }
        if (forged != null) {
            headers.set("X-Principal", forged);
        }
        return exchangeWithHeaders(HttpMethod.GET, "/headers/principal", headers, null);
    }

    private static String text(ResponseEntity<byte[]> response) {
        return new String(response.getBody(), StandardCharsets.UTF_8);
    }

    @Test
    void authenticatedPrincipalReachesRustInDirectMode() {
        ResponseEntity<byte[]> response = get("alice", null, null);
        assertEquals(200, response.getStatusCode().value(), text(response));
        assertEquals("alice [] ", text(response));
    }

    @Test
    void authenticatedPrincipalReachesRustInStreamingMode() {
        ResponseEntity<byte[]> response = get("bob", "application/x-ndjson", null);
        assertEquals(200, response.getStatusCode().value(), text(response));
        assertEquals("bob [] ", text(response));
    }

    @Test
    void anonymousRequestIsRejectedEvenWithAForgedHeader() {
        ResponseEntity<byte[]> response = get(null, null, "mallory");
        assertEquals(401, response.getStatusCode().value(), text(response));
        assertEquals("authentication required", text(response));
    }
}
