package com.devfive.vespera.bridge;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.http.InvalidMediaTypeException;
import org.springframework.http.MediaType;
import org.springframework.http.server.reactive.ServerHttpRequest;
import org.springframework.http.server.reactive.ServerHttpRequestDecorator;
import org.springframework.util.MultiValueMap;
import org.springframework.web.server.ResponseStatusException;
import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

import java.lang.reflect.Method;
import java.net.URLEncoder;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;

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
     * The form's {@code charset} parameter, or UTF-8 when it is absent or names a charset the
     * JVM does not know. The servlet twin ({@code FormBodySupport.requestCharset}) falls back the
     * same way; {@code MediaType.getCharset()} throws on an unknown name, which would otherwise
     * end the request as a generic {@code 500}.
     */
    static Charset formCharset(MediaType form) {
        try {
            return form.getCharset() != null ? form.getCharset() : StandardCharsets.UTF_8;
        } catch (IllegalArgumentException unsupported) {
            return StandardCharsets.UTF_8;
        }
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
        Charset charset = formCharset(form);
        return exchange.getFormData()
                .map(data -> {
                    byte[] rebuilt = encode(data, charset);
                    return rebuilt.length > body.length ? rebuilt : body;
                })
                .defaultIfEmpty(body)
                // reactor-netty rejects the second read of a body that a filter drained
                // without parsing it as a form: no form is cached, so there is nothing to
                // recover. Any other failure is a real read or parse error and propagates
                // instead of forwarding a truncated body.
                .onErrorResume(ReactiveFormBodySupport::isSecondReadRejection,
                        noCachedForm -> Mono.just(body));
    }

    /** The message of reactor-netty's second-read rejection (see {@link #secondMultipartRead}). */
    private static final String SECOND_READ_REJECTED = "Rejecting additional inbound receiver";

    private static boolean isSecondReadRejection(Throwable failure) {
        return failure instanceof IllegalStateException
                && failure.getMessage() != null
                && failure.getMessage().contains(SECOND_READ_REJECTED);
    }

    /** Body of the proxy's {@code 500} for a consumed multipart body (WebFlux). */
    static final String MULTIPART_CONSUMED_MSG = "vespera: the multipart request body was consumed before the "
            + "proxy could forward it (exchange.getMultipartData() in a WebFilter)";

    private static final Logger log = LoggerFactory.getLogger(ReactiveFormBodySupport.class);

    private static final AtomicBoolean MULTIPART_WARNED = new AtomicBoolean();

    /**
     * {@code body}, unless the request declares a {@code multipart/*} body
     * that came back short (empty, or shorter than the declared
     * {@code contentLength}; {@code 0} = declared empty): a {@code WebFilter}
     * consumed it through {@code exchange.getMultipartData()} (Spring
     * Security's {@code ServerCsrfTokenRequestAttributeHandler} with
     * {@code setTokenFromMultipartDataEnabled(true)}). Unlike a form, the raw
     * multipart body cannot be rebuilt faithfully, so — like the servlet
     * proxy ({@link MultipartBodySupport}) — this throws a {@code 500} with a
     * one-time WARN instead of forwarding an empty body to Rust.
     */
    static byte[] rejectConsumedMultipart(ServerWebExchange exchange, byte[] body, long contentLength) {
        boolean shortRead = contentLength != 0
                && (body.length == 0 || (contentLength > 0 && body.length < contentLength));
        if (!shortRead || !isMultipart(exchange) || framedBodyless(exchange, contentLength)) {
            return body;
        }
        String hint = "{} {}: multipart body was already consumed (exchange.getMultipartData()) before the "
                + "vespera proxy ran, so it cannot be forwarded to Rust; answering 500. Check for a WebFilter "
                + "reading multipart data (e.g. a CSRF token handler with setTokenFromMultipartDataEnabled(true)) "
                + "and exclude the proxied paths from it.";
        String method = exchange.getRequest().getMethod().name();
        String path = exchange.getRequest().getPath().value();
        if (MULTIPART_WARNED.compareAndSet(false, true)) {
            log.warn(hint, method, path);
        } else if (log.isDebugEnabled()) {
            log.debug(hint, method, path);
        }
        throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR, MULTIPART_CONSUMED_MSG);
    }

    /**
     * reactor-netty refuses a second read of a body a filter already drained
     * ({@code IllegalStateException: Rejecting additional inbound receiver}):
     * for a multipart request that is the consumed body
     * {@link #rejectConsumedMultipart} answers; any other failure propagates.
     */
    static Mono<byte[]> secondMultipartRead(ServerWebExchange exchange, IllegalStateException failure) {
        return isMultipart(exchange) ? Mono.just(VesperaWireCodec.EMPTY_BODY) : Mono.error(failure);
    }

    private static boolean isMultipart(ServerWebExchange exchange) {
        String contentType = exchange.getRequest().getHeaders().getFirst(HttpHeaders.CONTENT_TYPE);
        return contentType != null && contentType.strip().regionMatches(true, 0, "multipart/", 0, 10);
    }

    /**
     * The WebFlux twin of {@link MultipartBodySupport#framedBodyless}: no
     * {@code Content-Length} and no {@code Transfer-Encoding} on an HTTP/1.x
     * request means no body (RFC 9112 6.3), so an empty body is forwarded.
     * ServerHttpRequest does not expose the protocol, so it is read from the
     * native request (reactor-netty's {@code version()}, reflectively, so the
     * bridge keeps no reactor-netty compile dependency); an unknown protocol
     * keeps the 500, as HTTP/2 does.
     */
    private static boolean framedBodyless(ServerWebExchange exchange, long contentLength) {
        if (contentLength >= 0 || exchange.getRequest().getHeaders().getFirst(HttpHeaders.TRANSFER_ENCODING) != null) {
            return false;
        }
        return reactorHttpVersion(exchange.getRequest()).startsWith("HTTP/1.");
    }

    /**
     * The protocol text reactor-netty reports for {@code request} (for example
     * {@code HTTP/1.1}), or {@code ""} when it cannot be read: the decorators are
     * unwrapped to the reactor request, whose {@code getNativeRequest()} and the
     * native {@code version()} are called reflectively.
     */
    private static String reactorHttpVersion(ServerHttpRequest request) {
        try {
            ServerHttpRequest inner = request;
            while (inner instanceof ServerHttpRequestDecorator decorator) {
                inner = decorator.getDelegate();
            }
            Object nativeRequest = invokeNoArg(inner, "getNativeRequest");
            return nativeRequest == null ? "" : String.valueOf(invokeNoArg(nativeRequest, "version"));
        } catch (ReflectiveOperationException | RuntimeException unknown) {
            return "";
        }
    }

    private static Object invokeNoArg(Object target, String name) throws ReflectiveOperationException {
        Method method = target.getClass().getMethod(name);
        method.setAccessible(true);
        return method.invoke(target);
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
