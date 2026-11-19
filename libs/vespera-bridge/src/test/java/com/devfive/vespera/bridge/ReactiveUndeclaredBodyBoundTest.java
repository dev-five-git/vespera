package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.web.server.ResponseStatusException;

/**
 * The WebFlux body read of an undeclared-length request under an explicit cap: the
 * read is bounded by the lower of the cap and the frame bound. The frame bound is a
 * parameter here (production: {@code VesperaWireCodec.MAX_FRAME_BODY}, too large to
 * allocate in a unit test).
 */
class ReactiveUndeclaredBodyBoundTest {

    @Test
    void aBodyWithinTheFrameBoundIsAggregatedWhole() {
        byte[] body = VesperaReactiveProxyHandler.readBody(
                        MockServerHttpRequest.post("/x").body("1234"), -1, 100, 4)
                .block();

        assertArrayEquals("1234".getBytes(StandardCharsets.US_ASCII), body);
    }

    @Test
    void anUndeclaredBodyLongerThanTheFrameBoundIsRefusedUnderALargerCap() {
        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaReactiveProxyHandler.readBody(
                                MockServerHttpRequest.post("/x").body("12345"), -1, 100, 4)
                        .block());

        assertEquals(413, e.getStatusCode().value());
        assertTrue(e.getReason().contains("built-in 4-byte bound"), e.getReason());
    }

    @Test
    void aCapBelowTheFrameBoundStillNamesTheCap() {
        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaReactiveProxyHandler.readBody(
                                MockServerHttpRequest.post("/x").body("12345"), -1, 4, 100)
                        .block());

        assertEquals(413, e.getStatusCode().value());
        assertTrue(e.getReason().contains("max-buffered-request-bytes=4"), e.getReason());
    }
}
