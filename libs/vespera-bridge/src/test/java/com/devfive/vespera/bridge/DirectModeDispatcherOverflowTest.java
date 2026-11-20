package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;
import org.slf4j.LoggerFactory;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * A DIRECT overflow with {@code direct-retry-on-overflow=false} must not
 * answer a proxy 500 carrying the buffer size and the routing advice: the
 * dispatch fails instead, so the controller's failure path writes the
 * generic body and keeps the size and mode hint as the WARN cause.
 */
class DirectModeDispatcherOverflowTest {

    @Test
    void overflowWithRetryDisabledFailsTheDispatchAndWritesNothing() {
        DirectModeDispatcher dispatcher = new DirectModeDispatcher(false, (
                appName, method, path, query, headers, body, retry, virtualThread) -> {
            throw new VesperaBridge.BufferTooSmallException(4096);
        });
        ProxyExchange exchange = new ProxyExchange(
                LoggerFactory.getLogger(DirectModeDispatcherOverflowTest.class),
                List.of(), "GET", "request-1");
        exchange.mode = DispatchMode.DIRECT;
        MockHttpServletResponse response = new MockHttpServletResponse();

        VesperaBridge.BufferTooSmallException failure = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> dispatcher.dispatch(
                        response, "_default", "GET", "/big", null, null, null, false, exchange));

        assertEquals(4096, failure.requiredSize());
        assertTrue(failure.getMessage().contains("BIDIRECTIONAL_STREAMING"), failure.getMessage());
        assertFalse(response.isCommitted());
        assertEquals(0, response.getContentAsByteArray().length, "no proxy-written body");
    }
}
