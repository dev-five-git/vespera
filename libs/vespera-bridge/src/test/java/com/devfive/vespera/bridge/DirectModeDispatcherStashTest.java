package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.function.Consumer;
import org.junit.jupiter.api.Test;
import org.slf4j.LoggerFactory;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * A DIRECT request whose finished response is kept on the Rust side (an
 * event stream or an over-cap body) is streamed from the stash, and the
 * exchange must record the mode that actually ran: STREAMING.
 */
class DirectModeDispatcherStashTest {

    /** Fake stash that only records the calls; the streamed bytes are irrelevant here. */
    private static final class RecordingOps implements DirectStash.Ops {
        final List<String> calls = new CopyOnWriteArrayList<>();

        @Override
        public int fetch(long token, ByteBuffer out) {
            calls.add("fetch");
            return -1;
        }

        @Override
        public void stream(long token, Consumer<byte[]> headerConsumer, OutputStream out) {
            calls.add("stream");
        }

        @Override
        public void discard(long token) {
            calls.add("discard");
        }
    }

    @Test
    void aStashedOverflowIsRecordedAsStreamingNotDirect() throws Exception {
        RecordingOps ops = new RecordingOps();
        // The native side kept a finished event-stream response: no re-run,
        // the stash is streamed. The pooled seam throws what the real path throws.
        DirectModeDispatcher dispatcher = new DirectModeDispatcher(true, (
                appName, method, path, query, headers, body, retry, virtualThread) -> {
            throw new StashedOverflowException(100, 7L, true, ops);
        });
        List<DispatchEvent> events = new CopyOnWriteArrayList<>();
        ProxyExchange exchange = new ProxyExchange(
                LoggerFactory.getLogger(DirectModeDispatcherStashTest.class),
                List.of(events::add), "GET", "request-1");
        // The controller records DIRECT before the dispatch; the stash path must overwrite it.
        exchange.mode = DispatchMode.DIRECT;
        exchange.path = "/events";

        MockHttpServletResponse response = new MockHttpServletResponse();
        dispatcher.dispatch(
                response, "_default", "GET", "/events", null, null, null,
                false, exchange);
        exchange.complete(200, false);

        assertEquals(List.of("stream"), ops.calls, "streamed from the stash, handler not re-run");
        assertEquals(1, events.size());
        assertEquals(DispatchMode.STREAMING, events.get(0).mode());
    }
}
