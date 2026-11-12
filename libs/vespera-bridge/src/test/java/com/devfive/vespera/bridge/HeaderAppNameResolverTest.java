package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.http.HttpStatus;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.web.server.ResponseStatusException;
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

    private static MockHttpServletRequest selecting(String app) {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/users");
        req.addHeader("X-Vespera-App", app);
        return req;
    }

    @Test
    void allowlistAdmitsListedAppsAndTheDefaultApp() {
        HeaderAppNameResolver restricted =
                new HeaderAppNameResolver("X-Vespera-App", List.of(" public ", "reports"));
        assertTrue(restricted.isRestricted());
        assertEquals("public", restricted.resolveAppName(selecting("public")));
        assertEquals("reports", restricted.resolveAppName(selecting(" reports ")));
        assertEquals("_default", restricted.resolveAppName(selecting("_default")));
        assertNull(restricted.resolveAppName(new MockHttpServletRequest("GET", "/users")));
    }

    @Test
    void allowlistRejectsAnyOtherAppWithUnknownAppException() {
        HeaderAppNameResolver restricted = new HeaderAppNameResolver("X-Vespera-App", List.of("public"));
        UnknownAppException rejected =
                assertThrows(UnknownAppException.class, () -> restricted.resolveAppName(selecting("admin")));
        assertEquals("admin", rejected.appName());
        assertTrue(rejected.getMessage().contains("admin"));
    }

    @Test
    void emptyOrBlankAllowlistKeepsTheUnrestrictedBehaviour() {
        List<String> blanks = new ArrayList<>(Arrays.asList(" ", null, ""));
        for (HeaderAppNameResolver unrestricted : List.of(
                new HeaderAppNameResolver("X-Vespera-App", List.of()),
                new HeaderAppNameResolver("X-Vespera-App", blanks),
                new HeaderAppNameResolver("X-Vespera-App", null))) {
            assertFalse(unrestricted.isRestricted());
            // First non-default selection logs the one-time WARN, the second does not.
            assertEquals("admin", unrestricted.resolveAppName(selecting("admin")));
            assertEquals("admin", unrestricted.resolveAppName(selecting("admin")));
        }
    }

    @Test
    void selectionHeaderIsReportedLowerCaseForStripping() {
        assertEquals("x-vespera-app", resolver.selectionHeaderName());
        AppNameResolver custom = request -> null;
        assertNull(custom.selectionHeaderName());
    }

    @Test
    void proxyAnswersUnexposedAppWith404BeforeDispatch() throws Exception {
        VesperaProxyController controller = new VesperaProxyController(
                new HeaderAppNameResolver("X-Vespera-App", List.of("public")),
                request -> {
                    throw new AssertionError("mode must not be resolved for a rejected app");
                });
        MockHttpServletResponse response = new MockHttpServletResponse();
        assertNull(controller.proxy(selecting("admin"), response));
        assertEquals(HttpStatus.NOT_FOUND.value(), response.getStatus());
        assertEquals("text/plain;charset=utf-8", response.getContentType().replace(" ", ""));
        // The client-controlled app name is never echoed.
        assertEquals(VesperaProxyController.UNKNOWN_APP_MSG, response.getContentAsString());
    }

    @Test
    void forwardedHeadersDropTheSelectionHeaderAndFixContentLength() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/users");
        req.addHeader("X-Vespera-App", "admin");
        req.addHeader("Content-Length", "40");
        req.addHeader("Accept", "text/plain");

        Map<String, String> forwarded = new LinkedHashMap<>();
        VesperaProxyController.requestHeaders(req, "x-vespera-app", "3").writeTo(forwarded::put);
        assertEquals(Map.of("content-length", "3", "accept", "text/plain"), forwarded);

        Map<String, String> stripOnly = new LinkedHashMap<>();
        VesperaProxyController.requestHeaders(req, "x-vespera-app", null).writeTo(stripOnly::put);
        assertEquals(Map.of("content-length", "40", "accept", "text/plain"), stripOnly);

        Map<String, String> untouched = new LinkedHashMap<>();
        VesperaProxyController.requestHeaders(req, null, null).writeTo(untouched::put);
        assertEquals("admin", untouched.get("x-vespera-app"));
    }
}
