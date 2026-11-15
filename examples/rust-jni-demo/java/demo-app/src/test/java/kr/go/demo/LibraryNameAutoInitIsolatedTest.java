package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaBridgeNativeSettings;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;

/**
 * {@code vespera.bridge.library-name} loads the native library itself. In the
 * shared test JVM another class has always loaded it first (the setting is
 * then a no-op), so this class is tagged {@code isolated-jvm} and runs FIRST
 * in the {@code isolatedJvmTest} JVM, before anything calls
 * {@link VesperaBridge#init} — it must not extend
 * {@link AbstractVesperaHttpIntegrationTest}, whose static initializer would.
 * It never dispatches: the class after it forwards a native limit, which is
 * fixed on the first dispatch.
 */
@Tag("isolated-jvm")
@Order(1)
@SpringBootTest(
        classes = VesperaHttpTestApplication.class,
        webEnvironment = WebEnvironment.RANDOM_PORT,
        properties = {
            "vespera.bridge.library-name=rust_jni_demo",
            // The first context in a JVM aligns the native buffered-response
            // cap with this property (later contexts keep it), so it matches
            // the cap BufferedResponseCapIsolatedTest asserts next.
            "vespera.bridge.max-buffered-response-bytes=" + BufferedResponseCapIsolatedTest.CAP
        })
class LibraryNameAutoInitIsolatedTest {

    @Autowired
    private VesperaBridgeNativeSettings nativeSettings;

    @Test
    void libraryNameLoadsTheLibrary() {
        assertTrue(nativeSettings.autoInitialized(), "loaded by vespera.bridge.library-name");
        assertTrue(VesperaBridge.isInitialized());
    }
}
