package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

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
    void onlyAsciiWhitespaceIsTrimmedLikeRust() {
        // Rust trims ASCII whitespace only: a Unicode space is part of the
        // name (and Rust answers it 400), so the proxy must not "fix" it.
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Vespera-App", "\u2003admin\u2003");
        assertEquals("\u2003admin\u2003", resolver.resolveAppName(req));

        MockHttpServletRequest ascii = new MockHttpServletRequest("GET", "/x");
        ascii.addHeader("X-Vespera-App", "\r\n\u000cadmin\t ");
        assertEquals("admin", resolver.resolveAppName(ascii));

        MockHttpServletRequest vt = new MockHttpServletRequest("GET", "/x");
        vt.addHeader("X-Vespera-App", "\u000badmin");
        assertEquals("\u000badmin", resolver.resolveAppName(vt));
    }

    @Test
    void unicodeOnlyHeaderIsNotTheDefaultApp() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Vespera-App", "\u2003");
        assertEquals("\u2003", resolver.resolveAppName(req));
    }

    @Test
    void unrestrictedWarningNeverEchoesAClientNameVerbatim() {
        ch.qos.logback.classic.Logger logger = (ch.qos.logback.classic.Logger)
                org.slf4j.LoggerFactory.getLogger(HeaderAppNameResolver.class);
        ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent> warnings =
                new ch.qos.logback.core.read.ListAppender<>();
        warnings.start();
        logger.addAppender(warnings);
        try {
            HeaderAppNameResolver unrestricted = new HeaderAppNameResolver("X-Vespera-App");
            String overlong = "a".repeat(300);
            assertEquals(overlong, unrestricted.resolveAppName(selecting(overlong)));
            assertEquals(1, warnings.list.size());
            String logged = warnings.list.get(0).getFormattedMessage();
            assertFalse(logged.contains(overlong), logged);
            assertTrue(logged.contains("<invalid app name of 300 chars>"), logged);
        } finally {
            logger.detachAppender(warnings);
        }

        ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent> forged =
                new ch.qos.logback.core.read.ListAppender<>();
        forged.start();
        logger.addAppender(forged);
        try {
            HeaderAppNameResolver unrestricted = new HeaderAppNameResolver("X-Vespera-App");
            unrestricted.resolveAppName(selecting("admin\nWARN forged"));
            assertEquals(1, forged.list.size());
            String logged = forged.list.get(0).getFormattedMessage();
            assertFalse(logged.contains("forged"), logged);
            assertTrue(logged.contains("<invalid app name of 17 chars>"), logged);
        } finally {
            logger.detachAppender(forged);
        }
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
    void blankOnlyAllowlistFailsStartupInsteadOfOpeningTheProxy() {
        // ${ADMIN_APP:} with the variable unset binds to [""]: that must not
        // silently fall back to the unrestricted behaviour.
        for (List<String> blanks : List.of(
                Arrays.asList(""),
                Arrays.asList(" "),
                Arrays.asList("\t", null),
                Arrays.asList(" ", ""),
                List.<String>of())) {
            IllegalArgumentException failure = assertThrows(IllegalArgumentException.class,
                    () -> new HeaderAppNameResolver("X-Vespera-App", blanks), blanks.toString());
            assertTrue(failure.getMessage().contains("exposed-apps"), failure.getMessage());
        }
    }

    @Test
    void absentAllowlistKeepsTheUnrestrictedBehaviour() {
        ch.qos.logback.classic.Logger logger = (ch.qos.logback.classic.Logger)
                org.slf4j.LoggerFactory.getLogger(HeaderAppNameResolver.class);
        ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent> warnings =
                new ch.qos.logback.core.read.ListAppender<>();
        warnings.start();
        logger.addAppender(warnings);
        try {
            for (HeaderAppNameResolver unrestricted : List.of(
                    new HeaderAppNameResolver("X-Vespera-App", null),
                    new HeaderAppNameResolver("X-Vespera-App"))) {
                warnings.list.clear();
                assertFalse(unrestricted.isRestricted());
                // The default app never warns; the first non-default selection
                // logs the one-time WARN, the second does not.
                unrestricted.resolveAppName(selecting("_default"));
                assertEquals(0, warnings.list.size());
                assertEquals("admin", unrestricted.resolveAppName(selecting("admin")));
                assertEquals("admin", unrestricted.resolveAppName(selecting("admin")));
                assertEquals("public", unrestricted.resolveAppName(selecting("public")));
                assertEquals(1, warnings.list.size(), "logged once per resolver");
                ch.qos.logback.classic.spi.ILoggingEvent warning = warnings.list.get(0);
                assertEquals(ch.qos.logback.classic.Level.WARN, warning.getLevel());
                assertTrue(warning.getFormattedMessage().contains("vespera.bridge.exposed-apps is not set"),
                        warning.getFormattedMessage());
                assertTrue(warning.getFormattedMessage().contains("'admin'"), warning.getFormattedMessage());
            }
        } finally {
            logger.detachAppender(warnings);
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
