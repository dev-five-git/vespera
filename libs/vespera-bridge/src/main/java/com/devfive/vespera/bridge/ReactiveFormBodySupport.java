package com.devfive.vespera.bridge;

import org.springframework.http.InvalidMediaTypeException;
import org.springframework.http.MediaType;
import org.springframework.util.MultiValueMap;
import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

import java.net.URLEncoder;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

/**
 * The WebFlux counterpart of {@link FormBodySupport}: recovers an
 * {@code application/x-www-form-urlencoded} request body that a
 * {@code WebFilter} consumed before the proxy could read it.
 *
 * <p>{@link ServerWebExchange#getFormData()} reads the request body once and
 * caches the parsed form — Spring Security's {@code CsrfWebFilter} calls it
 * for every unsafe form request to look for the {@code _csrf} parameter. On
 * reactor-netty the body cannot be read a second time, so the proxy would
 * forward an empty body to Rust. The cached form survives, so the body is
 * rebuilt from it, re-encoded in the charset it was decoded with (the
 * {@code Content-Type} charset, else UTF-8 — Spring's
 * {@code FormHttpMessageReader} default).
 *
 * <p>Parameter order is preserved per name and by first appearance of each
 * name; interleaving across names ({@code a=1&b=2&a=3}) cannot be recovered
 * and comes back grouped ({@code a=1&a=3&b=2}).
 */
final class ReactiveFormBodySupport {

    private ReactiveFormBodySupport() {}

    /** The form content type, or {@code null} when the request is not a form-urlencoded body. */
    static MediaType formContentType(ServerWebExchange exchange) {
        MediaType contentType;
        try {
            contentType = exchange.getRequest().getHeaders().getContentType();
        } catch (InvalidMediaTypeException malformed) {
            return null;
        }
        return contentType != null
                && "application".equalsIgnoreCase(contentType.getType())
                && "x-www-form-urlencoded".equalsIgnoreCase(contentType.getSubtype())
                ? contentType : null;
    }

    /**
     * {@code body}, or — for a form body that came back short (empty, or
     * shorter than the declared {@code contentLength}; {@code -1} = unknown,
     * {@code 0} = declared empty, never recovered) — the body rebuilt
     * from {@link ServerWebExchange#getFormData()} when that is longer.
     */
    static Mono<byte[]> recoverConsumedBody(ServerWebExchange exchange, byte[] body, long contentLength) {
        boolean shortRead = contentLength != 0
                && (body.length == 0 || (contentLength > 0 && body.length < contentLength));
        MediaType form = shortRead ? formContentType(exchange) : null;
        if (form == null) {
            return Mono.just(body);
        }
        Charset charset = form.getCharset() != null ? form.getCharset() : StandardCharsets.UTF_8;
        return exchange.getFormData()
                .map(data -> {
                    byte[] rebuilt = encode(data, charset);
                    return rebuilt.length > body.length ? rebuilt : body;
                })
                .defaultIfEmpty(body)
                // Nothing to recover (e.g. the body was never read as a form).
                .onErrorReturn(body);
    }

    /** Re-encode {@code form} as an {@code application/x-www-form-urlencoded} body. */
    static byte[] encode(MultiValueMap<String, String> form, Charset charset) {
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, List<String>> entry : form.entrySet()) {
            String name = URLEncoder.encode(entry.getKey(), charset);
            for (String value : entry.getValue()) {
                if (out.length() > 0) {
                    out.append('&');
                }
                out.append(name);
                if (value != null) {
                    out.append('=').append(URLEncoder.encode(value, charset));
                }
            }
        }
        // The encoded form is pure ASCII, so the byte view is charset-independent.
        return out.toString().getBytes(StandardCharsets.US_ASCII);
    }
}
