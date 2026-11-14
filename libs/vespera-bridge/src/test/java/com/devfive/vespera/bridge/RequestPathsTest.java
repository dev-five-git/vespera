package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import jakarta.servlet.http.HttpServletMapping;
import jakarta.servlet.http.MappingMatch;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;
import org.springframework.mock.web.MockHttpServletMapping;
import org.springframework.mock.web.MockHttpServletRequest;

/** Servlet-path / context-path / path-parameter handling of the forwarded path. */
class RequestPathsTest {

    private static MockHttpServletRequest request(
            String uri, String context, String servletPath, MappingMatch match) {
        MockHttpServletRequest request = new MockHttpServletRequest("GET", uri);
        request.setContextPath(context);
        request.setServletPath(servletPath);
        if (match != null) {
            request.setHttpServletMapping(new MockHttpServletMapping("", "", "dispatcher", match));
        }
        return request;
    }

    @ParameterizedTest
    @CsvSource({
            // uri, context, servletPath, mapping, expected
            "/v1/health, '', /v1, PATH, /health",
            "/api/v1/users/7, /api, /v1, PATH, /users/7",
            "/v1, '', /v1, PATH, /",
            "/v1x/health, '', /v1, PATH, /v1x/health",
            // the default "/" mapping reports the whole path as servlet path: untouched
            "/health, '', /health, DEFAULT, /health",
            "/users/7, '', /users/7, EXACT, /users/7",
            "/api/health, /api, /health, , /health",
            // ;path parameters are dropped from every segment
            "/files;jsessionid=ABC/a;v=1/b, '', '', DEFAULT, /files/a/b",
            "/api;x=1/health;jsessionid=Z, /api, '', DEFAULT, /health",
    })
    void forwardsTheApplicationRelativePath(
            String uri, String context, String servletPath, MappingMatch match, String expected) {
        assertEquals(expected, VesperaProxyController.pathWithinApplication(
                request(uri, context, servletPath, match)));
    }

    /**
     * The servlet path is decoded and normalised by the container, the
     * request URI is not: the prefix is stripped by matching decoded raw
     * segments, so the forwarded remainder stays raw.
     */
    @ParameterizedTest
    @CsvSource({
            // uri, servletPath, expected
            "/%61pi/users, /api, /users",
            "/%61pi/users%2Fx, /api, /users%2Fx",
            "//api/users, /api, /users",
            "/api//users, /api, //users",
            "//api//v1/users, /api/v1, /users",
            // encodedSolidusHandling=decode: one raw segment holds two decoded ones
            "/api%2Fv1/users, /api/v1, /users",
            // encodedSolidusHandling=passthrough: %2F stays part of the remainder
            "/api/%2F/users, /api, /%2F/users",
            // dot segments the container resolved
            "/api/x/../users, /api, /users",
            "/x/../api/users, /api, /users",
            "/api/./users, /api, /users",
            "/%2e%2e/api/users, /api, /users",
            "/api;v=1/users, /api, /users",
            // non-ASCII servlet path
            "/%C3%A9t%C3%A9/users, /été, /users",
    })
    void prefixStripMatchesDecodedSegments(String uri, String servletPath, String expected) {
        assertEquals(expected, RequestPaths.pathWithinApplication(
                request(uri, "", servletPath, MappingMatch.PATH)));
    }

    @Test
    void unmatchablePrefixFallsBackToTheEncodedPathInfo() {
        MockHttpServletRequest request = request("/elsewhere/x", "", "/api", MappingMatch.PATH);
        request.setPathInfo("/us ers/é%");
        assertEquals("/us%20ers/%C3%A9%25", RequestPaths.pathWithinApplication(request));
    }

    @Test
    void containersWithoutMappingSupportAreTreatedAsUnmapped() {
        MockHttpServletRequest unsupported = new MockHttpServletRequest("GET", "/v1/health") {
            @Override
            public HttpServletMapping getHttpServletMapping() {
                throw new UnsupportedOperationException();
            }
        };
        unsupported.setServletPath("/v1");
        assertNull(RequestPaths.prefixMappedServletPath(unsupported));
        assertEquals("/v1/health", RequestPaths.pathWithinApplication(unsupported));

        MockHttpServletRequest noMapping = new MockHttpServletRequest("GET", "/v1/health") {
            @Override
            public HttpServletMapping getHttpServletMapping() {
                return null;
            }
        };
        assertNull(RequestPaths.prefixMappedServletPath(noMapping));
    }

    @Test
    void helpersHandleNullAndParameterFreeInput() {
        assertNull(RequestPaths.stripPathParameters(null));
        assertEquals("/plain", RequestPaths.stripPathParameters("/plain"));
        assertEquals("/a", RequestPaths.stripPrefix("/a", null));
        assertEquals("/a", RequestPaths.stripPrefix("/a", ""));
        assertEquals("/a", RequestPaths.stripPrefix("/a", "/b"));
    }
}
