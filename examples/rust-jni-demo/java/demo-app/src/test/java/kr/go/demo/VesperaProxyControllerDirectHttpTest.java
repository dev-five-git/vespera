package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.annotation.Bean;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

@SpringBootTest(
        classes = {
            VesperaHttpTestApplication.class,
            VesperaProxyControllerDirectHttpTest.DirectModeConfiguration.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = {
            "vespera.bridge.direct-retry-on-overflow=false",
            "logging.level.com.devfive.vespera.bridge.VesperaProxyController=DEBUG"
        })
class VesperaProxyControllerDirectHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class DirectModeConfiguration {
        @Bean
        DispatchModeResolver directDispatchModeResolver() {
            return request -> DispatchMode.DIRECT;
        }
    }

    @Test
    void customDirectResolverUsesPlatformThreadDirectOverload() {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals("ok", new String(response.getBody(), StandardCharsets.UTF_8));
        assertEquals(2, response.getHeaders().getContentLength());
    }

    @Test
    void unsafeDirectRequestsAreDowngradedToSyncEveryTime() {
        byte[] payload = "unsafe-direct".getBytes(StandardCharsets.UTF_8);

        ResponseEntity<byte[]> first = exchange(
                HttpMethod.POST, "/echo", MediaType.APPLICATION_OCTET_STREAM, payload);
        ResponseEntity<byte[]> second = exchange(
                HttpMethod.POST, "/echo", MediaType.APPLICATION_OCTET_STREAM, payload);

        assertEquals(HttpStatus.OK, first.getStatusCode());
        assertArrayEquals(payload, first.getBody());
        assertEquals(HttpStatus.OK, second.getStatusCode());
        assertArrayEquals(payload, second.getBody());
    }
}
