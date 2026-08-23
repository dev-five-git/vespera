package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

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
            VesperaProxyControllerBufferedLimitHttpTest.SyncModeConfiguration.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.max-buffered-request-bytes=4")
class VesperaProxyControllerBufferedLimitHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class SyncModeConfiguration {
        @Bean
        DispatchModeResolver syncDispatchModeResolver() {
            return request -> DispatchMode.SYNC;
        }
    }

    @Test
    void configuredBufferedRequestCapRejectsOversizedHttpBody() {
        ResponseEntity<byte[]> response = exchange(
                HttpMethod.POST,
                "/echo",
                MediaType.APPLICATION_OCTET_STREAM,
                "12345".getBytes(StandardCharsets.UTF_8));

        assertEquals(HttpStatus.PAYLOAD_TOO_LARGE, response.getStatusCode());
        assertNotNull(response.getBody());
        String body = new String(response.getBody(), StandardCharsets.UTF_8);
        assertTrue(body.contains("413"), body);
        assertTrue(body.contains("/echo"), body);
    }
}
