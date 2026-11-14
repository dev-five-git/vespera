package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaBridgeNativeSettings;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.web.server.LocalServerPort;

/**
 * {@code vespera.bridge.library-name} loads the native library itself. In the
 * shared test JVM another class has always loaded it first (the setting is
 * then a no-op), so this class is tagged {@code isolated-jvm} and runs in a
 * JVM of its own where nothing calls {@link VesperaBridge#init} — it must not
 * extend {@link AbstractVesperaHttpIntegrationTest}, whose static initializer
 * would.
 */
@Tag("isolated-jvm")
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.library-name=rust_jni_demo")
class LibraryNameAutoInitIsolatedTest {

    @Autowired
    private VesperaBridgeNativeSettings nativeSettings;

    @LocalServerPort
    private int port;

    @Test
    void libraryNameLoadsTheLibraryAndTheProxyServes() throws Exception {
        assertTrue(nativeSettings.autoInitialized(), "loaded by vespera.bridge.library-name");
        assertTrue(VesperaBridge.isInitialized());

        HttpResponse<String> response = HttpClient.newHttpClient().send(
                HttpRequest.newBuilder(URI.create("http://localhost:" + port + "/health")).build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, response.statusCode());
        assertEquals("ok", response.body());
    }
}
