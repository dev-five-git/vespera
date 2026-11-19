package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.Map;
import org.junit.jupiter.api.Test;

/** The fake transport's stream classification reads the Accept value only. */
class ReactiveTestSupportTest {

    private static byte[] request(String method, String path, Map<String, String> headers) {
        return VesperaWireCodec.encodeRequest("_default", method, path, "", headers, null);
    }

    @Test
    void anExplicitStreamAcceptIsClassifiedAsStreaming() {
        assertTrue(ReactiveTestSupport.FakeTransport.explicitStreamingRequest(
                request("GET", "/feed", Map.of("accept", "text/event-stream"))));
        assertTrue(ReactiveTestSupport.FakeTransport.explicitStreamingRequest(
                request("POST", "/feed", Map.of("accept", "application/x-ndjson"))));
    }

    @Test
    void aStreamWordInThePathOrMethodDoesNotMakeARequestExplicit() {
        assertFalse(ReactiveTestSupport.FakeTransport.explicitStreamingRequest(
                request("GET", "/ndjson-export", Map.of("accept", "application/json"))));
        assertFalse(ReactiveTestSupport.FakeTransport.explicitStreamingRequest(
                request("GET", "/event-stream/status", Map.of())));
        // The method is not a classifier either: a stream-like method with a JSON Accept.
        assertFalse(ReactiveTestSupport.FakeTransport.explicitStreamingRequest(
                request("EVENT-STREAM", "/status", Map.of("accept", "application/json"))));
    }
}
