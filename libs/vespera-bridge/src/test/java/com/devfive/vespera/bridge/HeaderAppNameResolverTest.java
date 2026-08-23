package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

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

        MockHttpServletRequest trailingOnly = new MockHttpServletRequest("GET", "/x");
        trailingOnly.addHeader("X-Vespera-App", "admin ");
        assertEquals("admin", resolver.resolveAppName(trailingOnly));
    }

    @Test
    void unicodeWhitespaceIsStripped() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Vespera-App", "\u2003admin\u2003");
        assertEquals("admin", resolver.resolveAppName(req));
    }

    @Test
    void constructorRejectsNullEmptyAndBlankHeaderNames() {
        assertThrows(IllegalArgumentException.class, () -> new HeaderAppNameResolver(null));
        assertThrows(IllegalArgumentException.class, () -> new HeaderAppNameResolver(""));
        assertThrows(IllegalArgumentException.class, () -> new HeaderAppNameResolver(" \t "));
    }

    @Test
    void alreadyTrimmedValueIsReturnedAndEmptyValueMeansDefaultApp() {
        MockHttpServletRequest named = new MockHttpServletRequest("GET", "/x");
        named.addHeader("X-Vespera-App", "admin");
        assertEquals("admin", resolver.resolveAppName(named));

        MockHttpServletRequest empty = new MockHttpServletRequest("GET", "/x");
        empty.addHeader("X-Vespera-App", "");
        assertNull(resolver.resolveAppName(empty));
    }
}
