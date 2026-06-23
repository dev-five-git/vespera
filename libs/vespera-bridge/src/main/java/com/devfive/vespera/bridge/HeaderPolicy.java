package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.BiConsumer;

final class HeaderPolicy {
    private HeaderPolicy() {}

    /**
     * Pure hop-by-hop response headers the proxy must NOT forward verbatim from
     * the Rust wire response. Forwarding a handler-supplied (or malicious
     * native) {@code transfer-encoding} / {@code connection} desynchronises
     * framing at the servlet container or a downstream proxy (e.g. a wire
     * {@code transfer-encoding: chunked} on a response the container frames with
     * {@code Content-Length}). These are connection-scoped per RFC 9110 and are
     * never legitimately emitted by an application handler.
     *
     * <p>{@code content-length} is not hop-by-hop by RFC semantics, but it is
     * proxy-owned in this servlet bridge: buffered/direct responses set the
     * exact bytes they write, and streaming responses let the servlet container
     * frame the body.
     *
     * <p>Names are compared case-insensitively against the canonical lowercase
     * form the wire header carries.
     */
    static boolean isHopByHopResponseHeader(String name) {
        return switch (name.length()) {
            case 2 -> name.regionMatches(true, 0, "te", 0, 2);
            case 7 -> name.regionMatches(true, 0, "trailer", 0, 7)
                    || name.regionMatches(true, 0, "upgrade", 0, 7);
            case 10 -> name.regionMatches(true, 0, "connection", 0, 10)
                    || name.regionMatches(true, 0, "keep-alive", 0, 10);
            case 17 -> name.regionMatches(true, 0, "transfer-encoding", 0, 17);
            case 18 -> name.regionMatches(true, 0, "proxy-authenticate", 0, 18);
            case 19 -> name.regionMatches(true, 0, "proxy-authorization", 0, 19);
            default -> false;
        };
    }

    /**
     * Apply a Rust wire response header to the servlet response, dropping the
     * hop-by-hop / framing headers the proxy owns ({@link #HOP_BY_HOP_RESPONSE_HEADERS}).
     */
    static void addServletResponseHeaders(
            HttpServletResponse response, ResponseHeaderAccumulator headers) {
        for (HeaderPair header : headers.headers) {
            addServletResponseHeader(response, header.name, header.value, headers.connectionTokens);
        }
    }

    static void addServletResponseHeader(
            HttpServletResponse response, String name, String value, Set<String> connectionTokens) {
        if (!isHopByHopResponseHeader(name)
                && !isContentLengthHeader(name)
                && !isConnectionNominatedHeader(name, connectionTokens)) {
            response.addHeader(name, value);
        }
    }

    static boolean isConnectionNominatedHeader(String name, Set<String> connectionTokens) {
        return connectionTokens != null && connectionTokens.contains(canonicalLowerHeaderName(name));
    }

    static boolean containsConnectionHeaderKey(byte[] wire, int off, int len) {
        int headersObject = findHeadersObjectStart(wire, off, len);
        return headersObject >= 0 && containsConnectionMemberName(wire, headersObject, off + len);
    }

    static boolean containsConnectionHeaderKey(ByteBuffer wire, int off, int len) {
        int headersObject = findHeadersObjectStart(wire, off, len);
        return headersObject >= 0 && containsConnectionMemberName(wire, headersObject, off + len);
    }

    private static int findHeadersObjectStart(byte[] wire, int off, int len) {
        int end = off + len - 10;
        for (int i = off; i <= end; i++) {
            if ((wire[i] & 0xFF) == '"' && isHeadersLiteralAt(wire, i + 1)) {
                int colon = skipJsonWhitespace(wire, i + 9, off + len);
                if (colon < off + len && (wire[colon] & 0xFF) == ':') {
                    int object = skipJsonWhitespace(wire, colon + 1, off + len);
                    if (object < off + len && (wire[object] & 0xFF) == '{') {
                        return object + 1;
                    }
                }
            }
        }
        return -1;
    }

    private static int findHeadersObjectStart(ByteBuffer wire, int off, int len) {
        int end = off + len - 10;
        for (int i = off; i <= end; i++) {
            if ((wire.get(i) & 0xFF) == '"' && isHeadersLiteralAt(wire, i + 1)) {
                int colon = skipJsonWhitespace(wire, i + 9, off + len);
                if (colon < off + len && (wire.get(colon) & 0xFF) == ':') {
                    int object = skipJsonWhitespace(wire, colon + 1, off + len);
                    if (object < off + len && (wire.get(object) & 0xFF) == '{') {
                        return object + 1;
                    }
                }
            }
        }
        return -1;
    }

    private static boolean containsConnectionMemberName(byte[] wire, int pos, int end) {
        boolean expectName = true;
        for (int i = pos; i < end; i++) {
            int b = wire[i] & 0xFF;
            if (b == '}') {
                return false;
            }
            if (expectName && b == '"' && isConnectionLiteralAt(wire, i + 1)) {
                int colon = skipJsonWhitespace(wire, i + 12, end);
                if (colon < end && (wire[colon] & 0xFF) == ':') {
                    return true;
                }
            }
            if (b == '"') {
                i = skipJsonString(wire, i + 1, end);
            } else if (b == ',') {
                expectName = true;
            } else if (b == ':') {
                expectName = false;
            }
        }
        return false;
    }

    private static boolean containsConnectionMemberName(ByteBuffer wire, int pos, int end) {
        boolean expectName = true;
        for (int i = pos; i < end; i++) {
            int b = wire.get(i) & 0xFF;
            if (b == '}') {
                return false;
            }
            if (expectName && b == '"' && isConnectionLiteralAt(wire, i + 1)) {
                int colon = skipJsonWhitespace(wire, i + 12, end);
                if (colon < end && (wire.get(colon) & 0xFF) == ':') {
                    return true;
                }
            }
            if (b == '"') {
                i = skipJsonString(wire, i + 1, end);
            } else if (b == ',') {
                expectName = true;
            } else if (b == ':') {
                expectName = false;
            }
        }
        return false;
    }

    private static int skipJsonWhitespace(byte[] wire, int pos, int end) {
        int p = pos;
        while (p < end) {
            int b = wire[p] & 0xFF;
            if (b != ' ' && b != '\n' && b != '\r' && b != '\t') {
                break;
            }
            p++;
        }
        return p;
    }

    private static int skipJsonWhitespace(ByteBuffer wire, int pos, int end) {
        int p = pos;
        while (p < end) {
            int b = wire.get(p) & 0xFF;
            if (b != ' ' && b != '\n' && b != '\r' && b != '\t') {
                break;
            }
            p++;
        }
        return p;
    }

    private static int skipJsonString(byte[] wire, int pos, int end) {
        for (int i = pos; i < end; i++) {
            int b = wire[i] & 0xFF;
            if (b == '\\') {
                i++;
            } else if (b == '"') {
                return i;
            }
        }
        return end;
    }

    private static int skipJsonString(ByteBuffer wire, int pos, int end) {
        for (int i = pos; i < end; i++) {
            int b = wire.get(i) & 0xFF;
            if (b == '\\') {
                i++;
            } else if (b == '"') {
                return i;
            }
        }
        return end;
    }

    private static boolean isHeadersLiteralAt(byte[] bytes, int pos) {
        return (bytes[pos] & 0xFF) == 'h'
                && (bytes[pos + 1] & 0xFF) == 'e'
                && (bytes[pos + 2] & 0xFF) == 'a'
                && (bytes[pos + 3] & 0xFF) == 'd'
                && (bytes[pos + 4] & 0xFF) == 'e'
                && (bytes[pos + 5] & 0xFF) == 'r'
                && (bytes[pos + 6] & 0xFF) == 's'
                && (bytes[pos + 7] & 0xFF) == '"';
    }

    private static boolean isHeadersLiteralAt(ByteBuffer bytes, int pos) {
        return (bytes.get(pos) & 0xFF) == 'h'
                && (bytes.get(pos + 1) & 0xFF) == 'e'
                && (bytes.get(pos + 2) & 0xFF) == 'a'
                && (bytes.get(pos + 3) & 0xFF) == 'd'
                && (bytes.get(pos + 4) & 0xFF) == 'e'
                && (bytes.get(pos + 5) & 0xFF) == 'r'
                && (bytes.get(pos + 6) & 0xFF) == 's'
                && (bytes.get(pos + 7) & 0xFF) == '"';
    }

    private static boolean isConnectionLiteralAt(byte[] bytes, int pos) {
        return (bytes[pos] & 0xFF) == 'c'
                && (bytes[pos + 1] & 0xFF) == 'o'
                && (bytes[pos + 2] & 0xFF) == 'n'
                && (bytes[pos + 3] & 0xFF) == 'n'
                && (bytes[pos + 4] & 0xFF) == 'e'
                && (bytes[pos + 5] & 0xFF) == 'c'
                && (bytes[pos + 6] & 0xFF) == 't'
                && (bytes[pos + 7] & 0xFF) == 'i'
                && (bytes[pos + 8] & 0xFF) == 'o'
                && (bytes[pos + 9] & 0xFF) == 'n'
                && (bytes[pos + 10] & 0xFF) == '"';
    }

    private static boolean isConnectionLiteralAt(ByteBuffer bytes, int pos) {
        return (bytes.get(pos) & 0xFF) == 'c'
                && (bytes.get(pos + 1) & 0xFF) == 'o'
                && (bytes.get(pos + 2) & 0xFF) == 'n'
                && (bytes.get(pos + 3) & 0xFF) == 'n'
                && (bytes.get(pos + 4) & 0xFF) == 'e'
                && (bytes.get(pos + 5) & 0xFF) == 'c'
                && (bytes.get(pos + 6) & 0xFF) == 't'
                && (bytes.get(pos + 7) & 0xFF) == 'i'
                && (bytes.get(pos + 8) & 0xFF) == 'o'
                && (bytes.get(pos + 9) & 0xFF) == 'n'
                && (bytes.get(pos + 10) & 0xFF) == '"';
    }

    record HeaderPair(String name, String value) {}

    static final class ResponseHeaderAccumulator implements BiConsumer<String, String> {
        final List<HeaderPair> headers = new ArrayList<>(8);
        Set<String> connectionTokens;

        @Override
        public void accept(String name, String value) {
            headers.add(new HeaderPair(name, value));
            if (name.length() == 10 && name.regionMatches(true, 0, "connection", 0, 10)) {
                connectionTokens = addConnectionTokens(connectionTokens, value);
            }
        }
    }

    static Set<String> addConnectionTokens(Set<String> tokens, String value) {
        int start = 0;
        int len = value.length();
        Set<String> result = tokens;
        while (start < len) {
            int comma = value.indexOf(',', start);
            int end = comma >= 0 ? comma : len;
            int tokenStart = trimHttpWhitespaceStart(value, start, end);
            int tokenEnd = trimHttpWhitespaceEnd(value, tokenStart, end);
            if (tokenStart < tokenEnd) {
                if (result == null) {
                    result = new HashSet<>(4);
                }
                result.add(canonicalLowerHeaderName(value.substring(tokenStart, tokenEnd)));
            }
            if (comma < 0) {
                break;
            }
            start = comma + 1;
        }
        return result;
    }

    private static int trimHttpWhitespaceStart(String value, int start, int end) {
        int p = start;
        while (p < end && isHttpWhitespace(value.charAt(p))) {
            p++;
        }
        return p;
    }

    private static int trimHttpWhitespaceEnd(String value, int start, int end) {
        int p = end;
        while (p > start && isHttpWhitespace(value.charAt(p - 1))) {
            p--;
        }
        return p;
    }

    private static boolean isHttpWhitespace(char c) {
        return c == ' ' || c == '\t';
    }

    static boolean isContentLengthHeader(String name) {
        return name.length() == 14 && name.regionMatches(true, 0, "content-length", 0, 14);
    }

    // Package-private (not private) so unit tests can verify duplicate-header
    // joining (B4) with MockHttpServletRequest.
    static Map<String, String> collectHeaders(HttpServletRequest request) {
        // Pre-size for a typical request header count so the common case
        // never resizes; keep LinkedHashMap (NOT HashMap) so insertion
        // order — and thus the request header JSON field order — stays
        // deterministic.
        Map<String, String> headers = new LinkedHashMap<>(32);
        forEachRequestHeader(request, headers::put);
        return headers;
    }

    static void forEachRequestHeader(HttpServletRequest request, VesperaBridge.HeaderSink sink) {
        Enumeration<String> names = request.getHeaderNames();
        // The Servlet spec permits getHeaderNames() to return null when the
        // container disallows header access; treat that as "no headers"
        // rather than letting a NullPointerException turn a recoverable case
        // into an HTTP 500.
        if (names == null) {
            return;
        }
        Map<String, String> merged = new LinkedHashMap<>(32);
        Set<String> connectionTokens = requestConnectionTokens(request);
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            String lowerName = canonicalLowerHeaderName(name);
            if (!isHopByHopRequestHeader(lowerName)
                    && !isConnectionNominatedHeader(lowerName, connectionTokens)) {
                String value = joinHeaderValues(name, request);
                merged.merge(lowerName, value, (left, right) ->
                        left + (lowerName.equals("cookie") ? "; " : ", ") + right);
            }
        }
        merged.forEach(sink::put);
    }

    private static Set<String> requestConnectionTokens(HttpServletRequest request) {
        Enumeration<String> values = request.getHeaders("Connection");
        Set<String> tokens = null;
        if (values == null) {
            return null;
        }
        while (values.hasMoreElements()) {
            tokens = addConnectionTokens(tokens, values.nextElement());
        }
        return tokens;
    }

    private static boolean isHopByHopRequestHeader(String name) {
        return isHopByHopResponseHeader(name);
    }

    /**
     * Combine every value of a repeated request header so duplicates are
     * not silently dropped before Rust sees them (the prior
     * {@code request.getHeader(name)} returned only the first value).
     *
     * <p>The single-value case — the overwhelming majority of headers —
     * returns the lone value with no allocation.  Multiple same-name
     * values are combined per RFC 7230 §3.2.2 with {@code ", "}, except
     * {@code Cookie}, whose values themselves contain commas and must be
     * joined with {@code "; "} per RFC 6265bis §5.4 so the Rust cookie
     * parser still receives a valid cookie string.
     */
    private static String joinHeaderValues(String name, HttpServletRequest request) {
        Enumeration<String> values = request.getHeaders(name);
        if (values == null || !values.hasMoreElements()) {
            // A non-conformant container can return an empty getHeaders(name)
            // AND a null getHeader(name) for a name that getHeaderNames()
            // listed; coalesce to "" so a null never reaches the wire-header
            // JSON encoder (VesperaWireCodec.writeJsonString) and NPEs there.
            String value = request.getHeader(name);
            return value != null ? value : "";
        }
        String first = values.nextElement();
        if (!values.hasMoreElements()) {
            return first;
        }
        String separator = name.equalsIgnoreCase("cookie") ? "; " : ", ";
        StringBuilder sb = new StringBuilder(first);
        do {
            sb.append(separator).append(values.nextElement());
        } while (values.hasMoreElements());
        return sb.toString();
    }

    /**
     * Lowercase an HTTP header name while avoiding per-request lowercase
     * allocations for common HTTP/1.1 canonical names. Header names are ASCII
     * per RFC 9110 §5.1, so uncommon names fall back to a small ASCII copy only
     * when they contain uppercase bytes.
     */
    private static String canonicalLowerHeaderName(String name) {
        switch (name) {
            case "Host": return "host";
            case "Content-Type": return "content-type";
            case "Content-Length": return "content-length";
            case "Accept": return "accept";
            case "Accept-Encoding": return "accept-encoding";
            case "Accept-Language": return "accept-language";
            case "Authorization": return "authorization";
            case "Connection": return "connection";
            case "Cookie": return "cookie";
            case "User-Agent": return "user-agent";
            case "Referer": return "referer";
            case "Origin": return "origin";
            case "Cache-Control": return "cache-control";
            case "If-None-Match": return "if-none-match";
            case "If-Modified-Since": return "if-modified-since";
            case "X-Forwarded-For": return "x-forwarded-for";
            case "X-Forwarded-Host": return "x-forwarded-host";
            case "X-Forwarded-Proto": return "x-forwarded-proto";
            case "X-Request-Id": return "x-request-id";
            // X-Vespera-App is the multi-app routing header sent on EVERY
            // request in multi-app deployments (the HeaderAppNameResolver
            // default); keep it on the allocation-free switch path instead of
            // falling through to a per-request char[]+String lowercase copy.
            case "X-Vespera-App": return "x-vespera-app";
            default: break;
        }
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c >= 'A' && c <= 'Z') {
                return toLowerCaseAscii(name);
            }
        }
        return name;
    }

    private static String toLowerCaseAscii(String name) {
        char[] chars = name.toCharArray();
        for (int i = 0; i < chars.length; i++) {
            char c = chars[i];
            if (c >= 'A' && c <= 'Z') {
                chars[i] = (char) (c + ('a' - 'A'));
            }
        }
        return new String(chars);
    }
}
