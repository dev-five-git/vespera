package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchModeResolver;
import com.devfive.vespera.bridge.VesperaProxyController;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
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
            VesperaProxyControllerDirectHttpTest.DirectModeConfiguration.class,
            DispatchRecorder.Config.class
        },
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = {
            "vespera.bridge.direct-retry-on-overflow=false",
            "logging.level.com.devfive.vespera.bridge.VesperaProxyController=DEBUG"
        })
class VesperaProxyControllerDirectHttpTest extends AbstractVesperaHttpIntegrationTest {

    @Autowired
    private DispatchRecorder recorder;

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
        assertEquals(DispatchMode.DIRECT, recorder.modeOf(response));
    }

    @Test
    void unsafeDirectRequestsAreDowngradedToSyncEveryTime() {
        byte[] payload = "unsafe-direct".getBytes(StandardCharsets.UTF_8);

        // The downgrade is logged on every request (WARN once per JVM, then
        // DEBUG, which this class enables): count the lines.
        ch.qos.logback.classic.Logger logger = (ch.qos.logback.classic.Logger)
                org.slf4j.LoggerFactory.getLogger(VesperaProxyController.class);
        ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent> lines =
                new ch.qos.logback.core.read.ListAppender<>();
        lines.start();
        logger.addAppender(lines);
        ResponseEntity<byte[]> first;
        ResponseEntity<byte[]> second;
        try {
            first = exchange(
                    HttpMethod.POST, "/echo", MediaType.APPLICATION_OCTET_STREAM, payload);
            second = exchange(
                    HttpMethod.POST, "/echo", MediaType.APPLICATION_OCTET_STREAM, payload);
        } finally {
            logger.detachAppender(lines);
        }
        assertEquals(2, lines.list.stream()
                .filter(line -> line.getFormattedMessage().contains("routed unsafe method POST to DIRECT")
                        && line.getFormattedMessage().contains("downgrading to SYNC"))
                .count(), "each unsafe request is downgraded");

        assertEquals(HttpStatus.OK, first.getStatusCode());
        assertArrayEquals(payload, first.getBody());
        assertEquals(HttpStatus.OK, second.getStatusCode());
        assertArrayEquals(payload, second.getBody());
    }
}
