package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;

class RequestShapeTest {

    @Test
    void httpMethodClassificationCoversEveryStandardMethodAndCase() {
        for (String method : new String[] {"GET", "head", "Put", "DELETE", "options"}) {
            assertTrue(HttpMethods.isIdempotent(method), method);
        }
        for (String method : new String[] {"GET", "head", "options"}) {
            assertTrue(HttpMethods.isSafe(method), method);
        }
        for (String method : new String[] {"POST", "PATCH", "CONNECT", "TRACE", ""}) {
            assertFalse(HttpMethods.isIdempotent(method), method);
            assertFalse(HttpMethods.isSafe(method), method);
        }
        assertFalse(HttpMethods.isIdempotent(null));
        assertFalse(HttpMethods.isSafe(null));
        assertFalse(HttpMethods.isSafe("PUT"));
        assertFalse(HttpMethods.isSafe("DELETE"));
    }

    @Test
    void captureCachesOneImmutableMetadataSnapshot() {
        MockHttpServletRequest request = new MockHttpServletRequest("GET", "/items");
        request.setProtocol("HTTP/1.1");

        RequestShape captured = RequestShape.capture(request);
        request.setMethod("POST");
        request.addHeader("Transfer-Encoding", "chunked");

        assertSame(captured, RequestShape.capture(request));
        assertSame(captured, RequestShape.from(request));
        assertTrue(RequestShape.definitelyBodyless(request));
        assertTrue(captured.definitelyBodyless);
        assertFalse(captured.transferEncodingPresent);
    }

    @Test
    void explicitContentLengthAndTransferEncodingDetermineBodylessness() {
        MockHttpServletRequest emptyPost = new MockHttpServletRequest("POST", "/items");
        emptyPost.setContent(new byte[0]);
        assertTrue(RequestShape.capture(emptyPost).definitelyBodyless);

        MockHttpServletRequest body = new MockHttpServletRequest("GET", "/items");
        body.setContent(new byte[] {1});
        assertFalse(RequestShape.capture(body).definitelyBodyless);

        MockHttpServletRequest chunked = new MockHttpServletRequest("GET", "/items");
        chunked.addHeader("Transfer-Encoding", "chunked");
        assertFalse(RequestShape.capture(chunked).definitelyBodyless);
    }

    @Test
    void unknownLengthRequiresHttpOneAndSafeMethod() {
        MockHttpServletRequest httpOneSafe = unknownLength("OPTIONS", "http/1.0");
        assertTrue(RequestShape.capture(httpOneSafe).definitelyBodyless);

        assertFalse(RequestShape.capture(unknownLength("POST", "HTTP/1.1")).definitelyBodyless);
        assertFalse(RequestShape.capture(unknownLength("GET", "HTTP/2")).definitelyBodyless);
        assertFalse(RequestShape.capture(unknownLength("GET", null)).definitelyBodyless);
    }

    private static MockHttpServletRequest unknownLength(String method, String protocol) {
        return new MockHttpServletRequest(method, "/items") {
            @Override
            public long getContentLengthLong() {
                return -1;
            }

            @Override
            public String getProtocol() {
                return protocol;
            }
        };
    }
}
