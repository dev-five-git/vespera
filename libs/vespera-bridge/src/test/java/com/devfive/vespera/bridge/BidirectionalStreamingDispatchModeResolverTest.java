package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;

/**
 * Gating tests for the default resolver's bodyless fast path:
 * provably bodyless requests skip the bidirectional request-pull
 * plumbing (response-only STREAMING, ~3x cheaper); anything that may
 * carry a body keeps full bidirectional streaming.
 */
class BidirectionalStreamingDispatchModeResolverTest {

    private final BidirectionalStreamingDispatchModeResolver resolver =
            new BidirectionalStreamingDispatchModeResolver();

    @Test
    void bodylessGetHeadOptionsUseResponseOnlyStreaming() {
        for (String method : new String[] {"GET", "HEAD", "OPTIONS"}) {
            MockHttpServletRequest req = new MockHttpServletRequest(method, "/x");
            assertEquals(DispatchMode.STREAMING, resolver.resolveMode(req), method);
        }
    }

    @Test
    void explicitZeroContentLengthUsesResponseOnlyStreamingForAnyMethod() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent(new byte[0]); // Content-Length: 0 — provably empty.
        assertEquals(DispatchMode.STREAMING, resolver.resolveMode(req));
    }

    @Test
    void requestWithBodyKeepsBidirectionalStreaming() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent(new byte[64]);
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, resolver.resolveMode(req));
    }

    @Test
    void lengthlessPostKeepsBidirectionalStreaming() {
        // No Content-Length on a method that may carry a body.
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, resolver.resolveMode(req));
    }

    @Test
    void chunkedGetKeepsBidirectionalStreaming() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Transfer-Encoding", "chunked");
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, resolver.resolveMode(req));
    }
}
