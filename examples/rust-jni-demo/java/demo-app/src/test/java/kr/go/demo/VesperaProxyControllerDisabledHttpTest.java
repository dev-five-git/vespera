package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaProxyController;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.context.ApplicationContext;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.controller-enabled=false")
class VesperaProxyControllerDisabledHttpTest extends AbstractVesperaHttpIntegrationTest {

    @Autowired
    private ApplicationContext context;

    @Test
    void disabledPropertyRemovesCatchAllAndLeavesHealthUnmapped() {
        assertTrue(context.getBeansOfType(VesperaProxyController.class).isEmpty());

        ResponseEntity<byte[]> response =
                exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.NOT_FOUND, response.getStatusCode());
        String body = response.getBody() == null
                ? ""
                : new String(response.getBody(), StandardCharsets.UTF_8);
        assertTrue(body.contains("404"), body);
        assertTrue(body.contains("/health"), body);
    }
}
