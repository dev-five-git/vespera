package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;

/**
 * A {@code CompletableFuture.complete} that throws must not leave a pending
 * Java exception on the Tokio worker that later completes another future.
 *
 * <p>Both completions must run on the same Tokio worker thread, so this class
 * is tagged {@code isolated-jvm}: the {@code isolatedJvmTest} JVM runs the
 * shared runtime with ONE worker ({@code vespera.runtime.workerThreads=1},
 * fixed when the runtime is built), and the test asserts the two completions
 * really shared a thread.
 */
@Tag("isolated-jvm")
@Order(3)
class AsyncDispatchExceptionHygieneTest {
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");
    private static final int TIMEOUT_SECONDS = 10;

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
    }

    @Test
    void throwingFutureCompleteDoesNotPoisonNextAsyncCompletion() throws Exception {
        Thread poisoned = poisonAsyncCompletion();

        AtomicReference<Thread> completedOn = new AtomicReference<>();
        CompletableFuture<byte[]> healthy = new CompletableFuture<>() {
            @Override
            public boolean complete(byte[] value) {
                completedOn.set(Thread.currentThread());
                return super.complete(value);
            }
        };
        VesperaBridge.dispatchAsync(healthy, healthRequest());

        byte[] wireResponse = healthy.get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
        assertEquals(200, VesperaBridge.decodeResponse(wireResponse).status());
        assertSame(poisoned, completedOn.get(), "both completions ran on the one worker thread");
    }

    private static Thread poisonAsyncCompletion() throws InterruptedException {
        CountDownLatch completeCalled = new CountDownLatch(1);
        AtomicInteger completeCalls = new AtomicInteger();
        AtomicReference<Thread> completedOn = new AtomicReference<>();
        CompletableFuture<byte[]> throwingFuture = new CompletableFuture<>() {
            @Override
            public boolean complete(byte[] value) {
                completedOn.set(Thread.currentThread());
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
        return completedOn.get();
    }

    private static byte[] healthRequest() {
        return VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
    }
}
