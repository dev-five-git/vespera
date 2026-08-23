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

/**
 * Boots the REAL {@link DemoApplication} — the class the READMEs tell users to
 * copy — rather than a test-only configuration, so the documented integration
 * is what gets verified.
 *
 * <p>This is the regression guard for the `@ComponentScan` trap: scanning
 * {@code com.devfive.vespera.bridge} makes Spring instantiate
 * {@code VesperaProxyController} as a plain {@code @RestController}, which has
 * no default constructor, and the context fails to start. The bridge must come
 * from autoconfiguration alone.
 */
@SpringBootTest(classes = DemoApplication.class, webEnvironment = WebEnvironment.RANDOM_PORT)
class DemoApplicationContextTest extends AbstractVesperaHttpIntegrationTest {

    @Autowired
    private ApplicationContext context;

    @Test
    void documentedApplicationStartsAndAutoconfiguresExactlyOneProxyController() {
        assertEquals(
                1,
                context.getBeanNamesForType(VesperaProxyController.class).length,
                "the proxy must be registered once, by autoconfiguration");
    }

    @Test
    void documentedApplicationProxiesToRust() {
        ResponseEntity<byte[]> response = exchange(HttpMethod.GET, "/health", null, null);

        assertEquals(HttpStatus.OK, response.getStatusCode());
        assertEquals(
                "ok",
                new String(
                        response.getBody() == null ? new byte[0] : response.getBody(),
                        StandardCharsets.UTF_8));
    }

    @Test
    void demoApplicationDoesNotComponentScanTheBridgePackage() {
        assertTrue(
                DemoApplication.class.getAnnotation(
                                org.springframework.context.annotation.ComponentScan.class)
                        == null,
                "re-adding @ComponentScan for the bridge package breaks startup");
    }
}
