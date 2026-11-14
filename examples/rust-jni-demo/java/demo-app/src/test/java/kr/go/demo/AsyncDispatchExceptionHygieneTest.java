package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;

/**
 * A {@code CompletableFuture.complete} that throws must not leave a pending
 * Java exception on the Tokio worker that later completes another future.
 *
 * <p>The runtime is pinned to ONE worker so the poisoned and the healthy
 * completion share a thread. The worker count is fixed for the process on the
 * first dispatch, so this class is tagged {@code isolated-jvm} and runs in a
 * JVM of its own ({@code isolatedJvmTest}, one JVM per class).
 */
@Tag("isolated-jvm")
class AsyncDispatchExceptionHygieneTest {
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");
    private static final int TIMEOUT_SECONDS = 10;

    @BeforeAll
    static void setUp() {
        assertTrue(VesperaBridge.configureRuntime(1, 0), "one worker, applied before the first dispatch");
        VesperaBridge.init("rust_jni_demo");
    }

    @Test
    void throwingFutureCompleteDoesNotPoisonNextAsyncCompletion() throws Exception {
        poisonAsyncCompletion();

        CompletableFuture<byte[]> healthy = new CompletableFuture<>();
        VesperaBridge.dispatchAsync(healthy, healthRequest());

        byte[] wireResponse = healthy.get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
        assertEquals(200, VesperaBridge.decodeResponse(wireResponse).status());
    }

    private static void poisonAsyncCompletion() throws InterruptedException {
        CountDownLatch completeCalled = new CountDownLatch(1);
        AtomicInteger completeCalls = new AtomicInteger();
        CompletableFuture<byte[]> throwingFuture = new CompletableFuture<>() {
            @Override
            public boolean complete(byte[] value) {
                completeCalls.incrementAndGet();
                completeCalled.countDown();
                throw new RuntimeException("intentional complete() failure");
            }
        };

        VesperaBridge.dispatchAsync(throwingFuture, healthRequest());

        assertTrue(
                completeCalled.await(TIMEOUT_SECONDS, TimeUnit.SECONDS),
                "poison future complete() must be invoked");
        assertEquals(1, completeCalls.get(), "poison future complete() call count");
    }

    private static byte[] healthRequest() {
        return VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
    }
}
