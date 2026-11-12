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
