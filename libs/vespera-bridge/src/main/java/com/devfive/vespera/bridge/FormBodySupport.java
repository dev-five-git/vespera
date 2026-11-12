package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

import java.net.URLDecoder;
import java.net.URLEncoder;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;

/**
 * Recovers {@code application/x-www-form-urlencoded} request bodies that the
 * servlet stack consumed before the proxy could read them.
 *
 * <p>Spring's {@code FormContentFilter} parses PUT/PATCH/DELETE form bodies
 * into request parameters, and any {@code getParameter} call (e.g. Spring
 * Security's {@code CsrfFilter}) makes the container parse a POST form body —
 * either way {@code getInputStream()} is drained by the time the proxy reads
 * it, and Rust would receive an empty body.  The parameters survive, so the
 * body is rebuilt from {@link HttpServletRequest#getParameterMap()} minus the
 * query-string parameters (the container lists those first for each name),
 * re-encoded in the charset they were decoded with — the request's character
 * encoding, else the decoder's default (ISO-8859-1 for container-parsed POST
 * bodies, UTF-8 for {@code FormContentFilter}) — so the bytes match the
 * original body.
 * Disabling {@code FormContentFilter} instead would change the host app's
 * behaviour, so the proxy repairs the body on its side.
 *
 * <p>Parameter order is preserved per name and by first appearance of each
 * name; interleaving across names ({@code a=1&b=2&a=3}) cannot be recovered
 * and comes back grouped ({@code a=1&a=3&b=2}).
 */
final class FormBodySupport {

    private static final String FORM_URLENCODED = "application/x-www-form-urlencoded";

    private FormBodySupport() {}

    /** Whether the request declares an {@code application/x-www-form-urlencoded} body. */
    static boolean isFormUrlEncoded(HttpServletRequest request) {
        String contentType = request.getContentType();
        return contentType != null
                && contentType.strip().regionMatches(true, 0, FORM_URLENCODED, 0, FORM_URLENCODED.length());
    }

    /**
     * Return {@code body}, or the form body rebuilt from the request parameters
     * when {@code body} is a form body the servlet stack already consumed
     * (empty, or shorter than the declared {@code contentLength}) and body
     * parameters are available.
     */
    static byte[] recoverConsumedBody(HttpServletRequest request, byte[] body, long contentLength) {
        boolean shortRead = body.length == 0 || (contentLength > 0 && body.length < contentLength);
        if (!shortRead || !isFormUrlEncoded(request)) {
            return body;
        }
        byte[] rebuilt = rebuildFromParameters(request);
        return rebuilt.length > body.length ? rebuilt : body;
    }

    /** Re-encode the body (non-query) parameters as a form-urlencoded body. */
    static byte[] rebuildFromParameters(HttpServletRequest request) {
        Charset charset = requestCharset(request);
        Map<String, Integer> queryCounts = queryParameterCounts(request.getQueryString());
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, String[]> entry : request.getParameterMap().entrySet()) {
            String name = entry.getKey();
            String[] values = entry.getValue();
            for (int i = queryCounts.getOrDefault(name, 0); i < values.length; i++) {
                if (out.length() > 0) {
                    out.append('&');
                }
                out.append(URLEncoder.encode(name, charset))
                        .append('=')
                        .append(URLEncoder.encode(values[i], charset));
            }
        }
        // The encoded form is pure ASCII, so the byte view is charset-independent.
        return out.toString().getBytes(StandardCharsets.US_ASCII);
    }

    static Charset requestCharset(HttpServletRequest request) {
        String encoding = request.getCharacterEncoding();
        if (encoding == null) {
            // POST params are parsed by the container (servlet default
            // ISO-8859-1); PUT/PATCH/DELETE ones only by Spring's
            // FormContentFilter, whose converter defaults to UTF-8.
            return "POST".equalsIgnoreCase(request.getMethod())
                    ? StandardCharsets.ISO_8859_1
                    : StandardCharsets.UTF_8;
        }
        try {
            return Charset.forName(encoding.strip());
        } catch (IllegalArgumentException unsupported) {
            return StandardCharsets.UTF_8;
        }
    }

    /** How many times each (decoded) parameter name occurs in the query string. */
    static Map<String, Integer> queryParameterCounts(String query) {
        Map<String, Integer> counts = new HashMap<>();
        if (query == null || query.isEmpty()) {
            return counts;
        }
        for (String pair : query.split("&")) {
            if (pair.isEmpty()) {
                continue;
            }
            int eq = pair.indexOf('=');
            String rawName = eq < 0 ? pair : pair.substring(0, eq);
            counts.merge(decodeName(rawName), 1, Integer::sum);
        }
        return counts;
    }

    private static String decodeName(String rawName) {
        try {
            return URLDecoder.decode(rawName, StandardCharsets.UTF_8);
        } catch (IllegalArgumentException malformed) {
            return rawName;
        }
    }
}
