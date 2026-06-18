package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;

class HeaderAppNameResolverTest {

    private final HeaderAppNameResolver resolver = new HeaderAppNameResolver("X-Vespera-App");

    @Test
    void missingOrBlankHeaderReturnsNull() {
        assertNull(resolver.resolveAppName(new MockHttpServletRequest("GET", "/x")));

        MockHttpServletRequest blank = new MockHttpServletRequest("GET", "/x");
        blank.addHeader("X-Vespera-App", "  \t ");
        assertNull(resolver.resolveAppName(blank));
    }

    @Test
    void nonBlankHeaderIsTrimmed() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Vespera-App", "  admin  ");
        assertEquals("admin", resolver.resolveAppName(req));
    }
}
