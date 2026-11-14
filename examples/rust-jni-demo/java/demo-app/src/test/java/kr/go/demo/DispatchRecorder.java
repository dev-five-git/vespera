package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import com.devfive.vespera.bridge.DispatchEvent;
import com.devfive.vespera.bridge.DispatchMode;
import com.devfive.vespera.bridge.DispatchObserver;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.annotation.Bean;
import org.springframework.http.ResponseEntity;

/**
 * Records every {@link DispatchEvent} so an HTTP test can assert the
 * {@link DispatchMode} the proxy really used (the response body alone is the
 * same on every dispatch path). Add {@link Config} to the
 * {@code @SpringBootTest} classes and autowire the recorder.
 */
final class DispatchRecorder implements DispatchObserver {

    @TestConfiguration(proxyBeanMethods = false)
    static class Config {
        @Bean
        DispatchRecorder dispatchRecorder() {
            return new DispatchRecorder();
        }
    }

    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();

    @Override
    public void onComplete(DispatchEvent event) {
        events.add(event);
    }

    /** The event of the response's request id (observers run after the response; waits up to 5 s). */
    DispatchEvent eventFor(ResponseEntity<?> response) {
        return eventFor(response.getHeaders().getFirst("X-Request-Id"));
    }

    DispatchEvent eventFor(String requestId) {
        assertNotNull(requestId, "response has no request id");
        long deadline = System.nanoTime() + 5_000_000_000L;
        do {
            for (DispatchEvent event : events) {
                if (requestId.equals(event.requestId())) {
                    return event;
                }
            }
            java.util.concurrent.locks.LockSupport.parkNanos(1_000_000L);
        } while (System.nanoTime() < deadline);
        throw new AssertionError("no DispatchEvent recorded for request id " + requestId);
    }

    DispatchMode modeOf(ResponseEntity<?> response) {
        return eventFor(response).mode();
    }
}
