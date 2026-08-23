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
            VesperaProxyControllerAsyncHttpTest.AsyncModeConfiguration.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT)
class VesperaProxyControllerAsyncHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class AsyncModeConfiguration {
        @Bean
        DispatchModeResolver asyncDispatchModeResolver() {
            return request -> DispatchMode.ASYNC;
        }
    }

    @Test
    void customAsyncResolverCompletesHttpResponseWithWireBodyResource() {
        ResponseEntity<byte[]> response =
                exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals("ok", new String(response.getBody(), StandardCharsets.UTF_8));
        assertNotNull(response.getHeaders().getContentType());
        assertTrue(response.getHeaders().getContentType().isCompatibleWith(MediaType.TEXT_PLAIN));
        assertEquals(2, response.getHeaders().getContentLength());
    }
}
