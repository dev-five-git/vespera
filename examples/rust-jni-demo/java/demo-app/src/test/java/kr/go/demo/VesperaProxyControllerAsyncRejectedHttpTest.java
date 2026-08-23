package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.Executor;
import java.util.concurrent.RejectedExecutionException;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.annotation.Bean;
import org.springframework.http.HttpMethod;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;

@SpringBootTest(
        classes = {
            VesperaHttpTestApplication.class,
            VesperaProxyControllerAsyncRejectedHttpTest.RejectedAsyncConfiguration.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT)
class VesperaProxyControllerAsyncRejectedHttpTest extends AbstractVesperaHttpIntegrationTest {

    @TestConfiguration(proxyBeanMethods = false)
    static class RejectedAsyncConfiguration {
        @Bean
        DispatchModeResolver asyncDispatchModeResolver() {
            return request -> DispatchMode.ASYNC;
        }

        @Bean("vesperaBridgeAsyncResponseExecutor")
        Executor rejectingAsyncResponseExecutor() {
            return command -> {
                throw new RejectedExecutionException("test executor saturated");
            };
        }
    }

    @Test
    void rejectedAsyncResponseBuildReturnsBackpressureResponse() {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.SERVICE_UNAVAILABLE, response.getStatusCode());
        assertEquals(
                "vespera: async response executor saturated",
                new String(response.getBody(), StandardCharsets.UTF_8));
    }
}
