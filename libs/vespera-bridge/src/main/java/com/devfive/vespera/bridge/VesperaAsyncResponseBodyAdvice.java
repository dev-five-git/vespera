package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import org.springframework.core.MethodParameter;
import org.springframework.core.io.InputStreamResource;
import org.springframework.http.HttpHeaders;
import org.springframework.http.MediaType;
import org.springframework.http.converter.HttpMessageConverter;
import org.springframework.http.server.ServerHttpRequest;
import org.springframework.http.server.ServerHttpResponse;
import org.springframework.http.server.ServletServerHttpRequest;
import org.springframework.http.server.ServletServerHttpResponse;
import org.springframework.web.bind.annotation.ControllerAdvice;
import org.springframework.web.servlet.mvc.method.annotation.ResponseBodyAdvice;

/**
 * Writes the body of every {@code ASYNC} proxy response itself, byte for
 * byte, like {@code SYNC} / {@code DIRECT} write straight to the servlet
 * response:
 *
 * <ul>
 *   <li>a body without a Rust {@code Content-Type} leaves without one —
 *       Spring's content negotiation would otherwise pick a type from the
 *       request's {@code Accept} header (a browser's {@code text/html});</li>
 *   <li>no message converter runs, so Spring never adds
 *       {@code Content-Disposition: inline;filename=f.txt} (its reflected
 *       file download guard for URLs such as {@code /files/report.pdf});</li>
 *   <li>the {@code ETag} / {@code Last-Modified} a {@code 200 GET} /
 *       {@code HEAD} entity held back (see
 *       {@code WireResponseWriter.buildAsyncResponseEntityFromWire}) are
 *       restored, so Spring's conditional-request check never turns a Rust
 *       {@code 200} into a {@code 304} / {@code 412}.</li>
 * </ul>
 *
 * <p>Only requests the proxy marked ({@link #markProxied}) and only
 * {@code byte[]} / {@code InputStreamResource} bodies are touched; every other
 * response body passes through unchanged. Registered by
 * {@link VesperaBridgeAutoConfiguration}; a hand-wired
 * {@link VesperaProxyController} needs this {@code @ControllerAdvice} bean
 * too, otherwise ASYNC responses fall back to Spring's message converters
 * (negotiated type, {@code Content-Disposition}) and lose their validators.
 */
@ControllerAdvice
public final class VesperaAsyncResponseBodyAdvice implements ResponseBodyAdvice<Object> {

    /** Request attribute marking a request the Vespera proxy answers asynchronously. */
    static final String PROXIED_ATTRIBUTE = VesperaAsyncResponseBodyAdvice.class.getName() + ".PROXIED";

    static void markProxied(HttpServletRequest request) {
        request.setAttribute(PROXIED_ATTRIBUTE, Boolean.TRUE);
    }

    /** Marks {@code request}, remembering the proxy's per-request response (deferred validators). */
    static void markProxied(HttpServletRequest request, ProxyServletResponse response) {
        request.setAttribute(PROXIED_ATTRIBUTE, response == null ? Boolean.TRUE : response);
    }

    @Override
    public boolean supports(MethodParameter returnType, Class<? extends HttpMessageConverter<?>> converterType) {
        return true;
    }

    @Override
    public Object beforeBodyWrite(Object body, MethodParameter returnType, MediaType selectedContentType,
            Class<? extends HttpMessageConverter<?>> selectedConverterType, ServerHttpRequest request,
            ServerHttpResponse response) {
        if (!(body instanceof byte[] || body != null && body.getClass() == InputStreamResource.class)
                || !(request instanceof ServletServerHttpRequest servlet)) {
            return body;
        }
        Object marker = servlet.getServletRequest().getAttribute(PROXIED_ATTRIBUTE);
        if (marker == null) {
            return body;
        }
        if (marker instanceof ProxyServletResponse proxied) {
            HttpHeaders validators = proxied.takeDeferredValidators(body);
            if (validators != null) {
                // A Content-Type Spring cannot parse goes on the servlet
                // response itself: ServletServerHttpResponse parses its
                // headers' Content-Type when it writes them.
                String rawContentType = validators.getFirst(HttpHeaders.CONTENT_TYPE);
                if (rawContentType != null && response instanceof ServletServerHttpResponse servletOut) {
                    validators = WireResponseWriter.withoutContentType(validators);
                    servletOut.getServletResponse().setHeader(HttpHeaders.CONTENT_TYPE, rawContentType);
                }
                HttpHeaders out = response.getHeaders();
                validators.forEach(out::addAll);
            }
        }
        try {
            // getBody() commits the entity's status and headers as they are.
            OutputStream out = response.getBody();
            if (body instanceof byte[] bytes) {
                out.write(bytes);
            } else {
                try (InputStream in = ((InputStreamResource) body).getInputStream()) {
                    in.transferTo(out);
                }
            }
        } catch (IOException failure) {
            throw new UncheckedIOException(failure);
        }
        // Nothing left for a message converter to write.
        return null;
    }
}
