package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import java.time.Duration;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/**
 * Child-JVM probe for {@link RobustnessE2ETest#shutdownDrainsAndCompletesAsyncFutures}:
 * the runtime shutdown is irreversible for the process, so it cannot run in
 * the shared test JVM.  Prints
 * {@code RESULT <shutdown> <slow> <stuck> <after> <afterAsync>}.
 */
public final class ShutdownProbe {

    private ShutdownProbe() {}

    private static byte[] get(String path) {
        return VesperaBridge.encodeRequest("GET", path, null, Map.of(), null);
    }

    private static int status(byte[] wire) {
        return VesperaBridge.decodeResponse(wire).status();
    }

    public static void main(String[] args) throws Exception {
        VesperaBridge.init("rust_jni_demo");
        // Warm the runtime so both dispatches are in flight before shutdown.
        status(VesperaBridge.dispatchBytes(get("/health")));
        CompletableFuture<byte[]> slow = VesperaBridge.dispatch(get("/stream/slow"));
        CompletableFuture<byte[]> stuck = VesperaBridge.dispatch(get("/stream/stuck"));
        Thread.sleep(100);
        boolean shutdown = VesperaBridge.shutdown(Duration.ofSeconds(2));
        int slowStatus = status(slow.get(10, TimeUnit.SECONDS));
        int stuckStatus = status(stuck.get(10, TimeUnit.SECONDS));
        int after = status(VesperaBridge.dispatchBytes(get("/health")));
        int afterAsync = status(VesperaBridge.dispatch(get("/health")).get(10, TimeUnit.SECONDS));
        System.out.println("RESULT " + shutdown + " " + slowStatus + " " + stuckStatus + " " + after
                + " " + afterAsync);
        System.exit(0);
    }
}
