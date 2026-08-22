package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

class HeaderPolicyTest {

    @Test
    void responseHeaderClassifierCoversEverySpecialCaseCaseInsensitively() {
        for (String name : new String[] {
                "TE", "Trailer", "Upgrade", "Connection", "Keep-Alive",
                "Transfer-Encoding", "Proxy-Authenticate", "Proxy-Authorization"
        }) {
            assertTrue(HeaderPolicy.isHopByHopResponseHeader(name), name);
            assertTrue(HeaderPolicy.isHopByHopResponseHeader(name.toLowerCase()), name);
        }

        assertFalse(HeaderPolicy.isHopByHopResponseHeader("Host"));
        assertFalse(HeaderPolicy.isHopByHopResponseHeader("Content-Length"));
        assertFalse(HeaderPolicy.isHopByHopResponseHeader("x-application-header"));
        assertTrue(HeaderPolicy.isContentLengthHeader("CONTENT-LENGTH"));
        assertFalse(HeaderPolicy.isContentLengthHeader("Content-Type"));
    }

    @Test
    void responseAccumulatorDropsStaticDynamicAndProxyOwnedHeaders() {
        HeaderPolicy.ResponseHeaderAccumulator accumulator =
                new HeaderPolicy.ResponseHeaderAccumulator();
        accumulator.accept("Connection", " X-Debug , x-secret");
        accumulator.accept("connection", "X-Second-Hop");
        accumulator.accept("X-Debug", "drop-one");
        accumulator.accept("x-secret", "drop-two");
        accumulator.accept("X-Second-Hop", "drop-three");
        accumulator.accept("Transfer-Encoding", "chunked");
        accumulator.accept("Content-Length", "999");
        accumulator.accept("Host", "upstream.example");
        accumulator.accept("Set-Cookie", "a=1");
        accumulator.accept("Set-Cookie", "b=2");

        MockHttpServletResponse response = new MockHttpServletResponse();
        HeaderPolicy.addServletResponseHeaders(response, accumulator);

        assertFalse(response.containsHeader("Connection"));
        assertFalse(response.containsHeader("X-Debug"));
        assertFalse(response.containsHeader("x-secret"));
        assertFalse(response.containsHeader("X-Second-Hop"));
        assertFalse(response.containsHeader("Transfer-Encoding"));
        assertFalse(response.containsHeader("Content-Length"));
        assertEquals("upstream.example", response.getHeader("Host"));
        assertEquals(List.of("a=1", "b=2"), response.getHeaders("Set-Cookie"));

        HeaderPolicy.HeaderPair pair = accumulator.headers.get(0);
        assertEquals("Connection", pair.name());
        assertEquals(" X-Debug , x-secret", pair.value());
        assertTrue(HeaderPolicy.isConnectionNominatedHeader("X-DEBUG", accumulator.connectionTokens));
        assertFalse(HeaderPolicy.isConnectionNominatedHeader("X-Other", accumulator.connectionTokens));
        assertFalse(HeaderPolicy.isConnectionNominatedHeader("X-Debug", null));
    }

    @Test
    void connectionTokenParserTrimsBoundsAndLimitsWork() {
        Set<String> existing = new HashSet<>(Set.of("existing"));
        Set<String> parsed = HeaderPolicy.addConnectionTokens(
                existing,
                "\t Foo \t, ,BAR," + "X".repeat(129) + "," + "Y".repeat(128));

        assertSame(existing, parsed);
        assertTrue(parsed.containsAll(Set.of("existing", "foo", "bar", "y".repeat(128))));
        assertFalse(parsed.contains("x".repeat(129)));
        assertNull(HeaderPolicy.addConnectionTokens(null, ""));
        assertNull(HeaderPolicy.addConnectionTokens(null, " \t ,\t"));

        StringBuilder many = new StringBuilder();
        for (int i = 0; i < 33; i++) {
            if (i > 0) {
                many.append(',');
            }
            many.append("Token").append(i);
        }
        Set<String> limited = HeaderPolicy.addConnectionTokens(null, many.toString());
        assertEquals(32, limited.size());
        assertTrue(limited.contains("token31"));
        assertFalse(limited.contains("token32"));
    }

    @Test
    void requestHeadersForwardEndToEndHeadersAndDropAllHopHeaders() {
        List<String> names = new ArrayList<>(List.of(
                "Host", "Content-Type", "Content-Length", "Accept", "Accept-Encoding",
                "Accept-Language", "Authorization", "Connection", "Cookie", "User-Agent",
                "Referer", "Origin", "Cache-Control", "If-None-Match", "If-Modified-Since",
                "X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto", "X-Request-Id",
                "X-Vespera-App", "X-CuStOm", "already-lower",
                "TE", "Trailer", "Upgrade", "Keep-Alive", "Transfer-Encoding",
                "Proxy-Authenticate", "Proxy-Authorization", "X-Dynamic-Hop"));
        ScriptedRequest request = new ScriptedRequest(names);
        for (String name : names) {
            request.values(name, name + "-value");
        }
        request.values("Connection", "X-Dynamic-Hop");

        Map<String, String> headers = HeaderPolicy.collectHeaders(request);

        for (String expected : List.of(
                "host", "content-type", "content-length", "accept", "accept-encoding",
                "accept-language", "authorization", "cookie", "user-agent", "referer",
                "origin", "cache-control", "if-none-match", "if-modified-since",
                "x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-request-id",
                "x-vespera-app", "x-custom", "already-lower")) {
            assertTrue(headers.containsKey(expected), expected);
        }
        for (String dropped : List.of(
                "connection", "te", "trailer", "upgrade", "keep-alive",
                "transfer-encoding", "proxy-authenticate", "proxy-authorization",
                "x-dynamic-hop")) {
            assertFalse(headers.containsKey(dropped), dropped);
        }
    }

    @Test
    void duplicateRawNamesMergeCaseInsensitivelyWithCorrectSeparators() {
        ScriptedRequest request = new ScriptedRequest(List.of(
                "h0", "h1", "h2", "h3", "h4", "h5", "h6", "h7",
                "X-Ninth", "x-ninth", "Cookie", "cookie"));
        for (int i = 0; i < 8; i++) {
            request.values("h" + i, "v" + i);
        }
        request.values("X-Ninth", "left");
        request.values("x-ninth", "right");
        request.values("Cookie", "a=1");
        request.values("cookie", "b=2");

        Map<String, String> headers = HeaderPolicy.collectHeaders(request);

        assertEquals("left, right", headers.get("x-ninth"));
        assertEquals("a=1; b=2", headers.get("cookie"));
        for (int i = 0; i < 8; i++) {
            assertEquals("v" + i, headers.get("h" + i));
        }
    }

    @Test
    void duplicateWithinFirstEightNamesTakesMergedPath() {
        ScriptedRequest request = new ScriptedRequest(List.of("Accept", "accept"));
        request.values("Accept", "text/html");
        request.values("accept", "application/json");

        assertEquals("text/html, application/json",
                HeaderPolicy.collectHeaders(request).get("accept"));
    }

    @Test
    void duplicateDetectionRecognizesEveryInlineSeenSlot() {
        for (int duplicateIndex = 0; duplicateIndex < 8; duplicateIndex++) {
            List<String> names = new ArrayList<>();
            for (int i = 0; i < 8; i++) {
                names.add("X-H" + i);
            }
            names.add("x-h" + duplicateIndex);
            ScriptedRequest request = new ScriptedRequest(names);
            for (String name : names) {
                request.values(name, name);
            }

            Map<String, String> headers = HeaderPolicy.collectHeaders(request);
            assertEquals("X-H" + duplicateIndex + ", x-h" + duplicateIndex,
                    headers.get("x-h" + duplicateIndex));
        }
    }

    @Test
    void mergedPathStillFiltersStaticAndConnectionNominatedHeaders() {
        ScriptedRequest request = new ScriptedRequest(List.of(
                "Accept", "accept", "Connection", "TE", "X-Hop"));
        request.values("Accept", "text/html");
        request.values("accept", "application/json");
        request.values("Connection", "X-Hop");
        request.values("TE", "trailers");
        request.values("X-Hop", "secret");

        Map<String, String> headers = HeaderPolicy.collectHeaders(request);

        assertEquals(Map.of("accept", "text/html, application/json"), headers);
    }

    @Test
    void threeValuesExerciseRepeatedJoinLoop() {
        ScriptedRequest request = new ScriptedRequest(List.of("Accept"));
        request.values("Accept", "text/html", "application/json", "text/plain");

        assertEquals("text/html, application/json, text/plain",
                HeaderPolicy.collectHeaders(request).get("accept"));
    }

    @Test
    void nonConformantServletEnumerationsAreHandledWithoutNulls() {
        ScriptedRequest noNames = new ScriptedRequest((List<String>) null);
        assertTrue(HeaderPolicy.collectHeaders(noNames).isEmpty());

        ScriptedRequest namesDisappear =
                new ScriptedRequest(List.of("X-One"), null).nullConnectionHeaders();
        namesDisappear.values("X-One", "one");
        assertTrue(HeaderPolicy.collectHeaders(namesDisappear).isEmpty());

        ScriptedRequest mergedNamesDisappear =
                new ScriptedRequest(List.of("X-One", "x-one"), null);
        mergedNamesDisappear.values("X-One", "one");
        mergedNamesDisappear.values("x-one", "two");
        assertTrue(HeaderPolicy.collectHeaders(mergedNamesDisappear).isEmpty());

        ScriptedRequest missingValues =
                new ScriptedRequest(List.of("X-Fallback", "X-Empty"));
        missingValues.nullValues("X-Fallback").fallback("X-Fallback", "fallback");
        missingValues.emptyValues("X-Empty");
        Map<String, String> headers = HeaderPolicy.collectHeaders(missingValues);
        assertEquals("fallback", headers.get("x-fallback"));
        assertEquals("", headers.get("x-empty"));
    }

    private static final class ScriptedRequest extends MockHttpServletRequest {
        private final List<List<String>> nameCalls;
        private final Map<String, List<String>> values = new LinkedHashMap<>();
        private final Set<String> nullValues = new HashSet<>();
        private final Set<String> emptyValues = new HashSet<>();
        private final Map<String, String> fallbacks = new HashMap<>();
        private int nameCall;
        private boolean nullConnectionHeaders;

        @SafeVarargs
        ScriptedRequest(List<String>... nameCalls) {
            super("GET", "/x");
            this.nameCalls = new ArrayList<>(Arrays.asList(nameCalls));
        }

        ScriptedRequest values(String name, String... headerValues) {
            values.put(name, List.of(headerValues));
            return this;
        }

        ScriptedRequest nullValues(String name) {
            nullValues.add(name);
            return this;
        }

        ScriptedRequest emptyValues(String name) {
            emptyValues.add(name);
            return this;
        }

        ScriptedRequest fallback(String name, String value) {
            fallbacks.put(name, value);
            return this;
        }

        ScriptedRequest nullConnectionHeaders() {
            nullConnectionHeaders = true;
            return this;
        }

        @Override
        public Enumeration<String> getHeaderNames() {
            List<String> names = nameCall < nameCalls.size()
                    ? nameCalls.get(nameCall++)
                    : nameCalls.get(nameCalls.size() - 1);
            return names == null ? null : Collections.enumeration(names);
        }

        @Override
        public Enumeration<String> getHeaders(String name) {
            if (nullConnectionHeaders && name.equalsIgnoreCase("Connection")) {
                return null;
            }
            if (nullValues.contains(name)) {
                return null;
            }
            if (emptyValues.contains(name)) {
                return Collections.emptyEnumeration();
            }
            List<String> exact = values.get(name);
            if (exact != null) {
                return Collections.enumeration(exact);
            }
            if (name.equalsIgnoreCase("Connection")) {
                List<String> connections = new ArrayList<>();
                values.forEach((key, entryValues) -> {
                    if (key.equalsIgnoreCase("Connection")) {
                        connections.addAll(entryValues);
                    }
                });
                return Collections.enumeration(connections);
            }
            return Collections.emptyEnumeration();
        }

        @Override
        public String getHeader(String name) {
            return fallbacks.get(name);
        }
    }
}
