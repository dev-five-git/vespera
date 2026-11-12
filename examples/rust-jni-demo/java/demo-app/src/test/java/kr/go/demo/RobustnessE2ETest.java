package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/** Native robustness limits and the graceful runtime shutdown, end to end. */
class RobustnessE2ETest {

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static int statusWithHeaders(int count) {
        Map<String, String> headers = new LinkedHashMap<>();
        for (int i = 0; i < count; i++) {
            headers.put("x-h-" + i, "v");
        }
        byte[] wire = VesperaBridge.encodeRequest("GET", "/health", null, headers, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
    }

    @Test
    void tooManyRequestHeadersAnswer431() {
        assertEquals(200, statusWithHeaders(500));
        assertEquals(431, statusWithHeaders(1025));
        // Far beyond http::HeaderMap's own limit (used to panic → 500).
        assertEquals(431, statusWithHeaders(40_000));
    }

    @Test
    void shutdownDrainsAndCompletesAsyncFutures() throws Exception {
        String java = ProcessHandle.current().info().command().orElse("java");
        List<String> command = new ArrayList<>(List.of(
                java, "-cp", System.getProperty("java.class.path")));
        String nativeDir = System.getProperty("vespera.native.dir");
        if (nativeDir != null) {
            command.add("-Dvespera.native.dir=" + nativeDir);
        }
        command.add(ShutdownProbe.class.getName());
        Process child = new ProcessBuilder(command).redirectErrorStream(true).start();
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        try (InputStream in = child.getInputStream()) {
            in.transferTo(output);
        }
        assertTrue(child.waitFor(60, TimeUnit.SECONDS), "probe JVM did not exit");
        String text = output.toString(StandardCharsets.UTF_8);
        // In-flight slow dispatch drained (200), the stuck one completed with
        // 503 when the runtime dropped it, later dispatches answer 503.
        assertTrue(text.contains("RESULT true 200 503 503"), text);
    }
}
