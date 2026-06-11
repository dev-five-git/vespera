package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

/** Pure-Java gating tests for {@link SmartDispatchModeResolver}. */
class SmartDispatchModeResolverTest {

    private final SmartDispatchModeResolver resolver = new SmartDispatchModeResolver();

    private static HttpServletRequest request(String method, long contentLength) {
        MockHttpServletRequest req = new MockHttpServletRequest(method, "/x");
        if (contentLength >= 0) {
            // MockHttpServletRequest derives getContentLengthLong() from
            // the content array length, not the header.
            req.setContent(new byte[(int) contentLength]);
        }
        return req;
    }

    @Test
    void smallIdempotentRequestUsesDirect() {
        assertEquals(DispatchMode.DIRECT,
                resolver.resolveMode(request("GET", 128)));
        assertEquals(DispatchMode.DIRECT,
                resolver.resolveMode(request("DELETE", 0)));
        assertEquals(DispatchMode.DIRECT,
                resolver.resolveMode(request("PUT",
                        SmartDispatchModeResolver.DEFAULT_MAX_DIRECT_BYTES)));
    }

    @Test
    void smallNonIdempotentRequestsUseSyncNeverDirect() {
        // SYNC never re-runs the handler — safe for POST/PATCH, and
        // 7.5x cheaper than bidirectional streaming for small bodies.
        assertEquals(DispatchMode.SYNC,
                resolver.resolveMode(request("POST", 128)));
        assertEquals(DispatchMode.SYNC,
                resolver.resolveMode(request("PATCH", 128)));
    }

    @Test
    void bodylessGetWithoutContentLengthUsesDirect() {
        // The common GET shape: no body, no Content-Length header.
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        assertEquals(DispatchMode.DIRECT, resolver.resolveMode(req));
    }

    @Test
    void chunkedTransferEncodingFallsBackToStreaming() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Transfer-Encoding", "chunked");
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, resolver.resolveMode(req));
    }

    @Test
    void lengthlessNonIdempotentFallsBackToStreaming() {
        // POST without Content-Length: body cannot be ruled out.
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING, resolver.resolveMode(req));
    }

    @Test
    void oversizedNonIdempotentFallsBackToStreaming() {
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING,
                resolver.resolveMode(request("POST",
                        SmartDispatchModeResolver.DEFAULT_MAX_DIRECT_BYTES + 1)));
    }

    @Test
    void oversizedRequestFallsBackToStreaming() {
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING,
                resolver.resolveMode(request("GET",
                        SmartDispatchModeResolver.DEFAULT_MAX_DIRECT_BYTES + 1)));
    }

    @Test
    void customCapIsHonoured() {
        SmartDispatchModeResolver tight = new SmartDispatchModeResolver(64);
        assertEquals(DispatchMode.DIRECT, tight.resolveMode(request("GET", 64)));
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING,
                tight.resolveMode(request("GET", 65)));
    }

    @Test
    void negativeCapRejected() {
        assertThrows(IllegalArgumentException.class,
                () -> new SmartDispatchModeResolver(-1));
    }
}
