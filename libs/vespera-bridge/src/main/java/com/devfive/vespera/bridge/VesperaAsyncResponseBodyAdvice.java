package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import org.springframework.core.MethodParameter;
import org.springframework.core.io.InputStreamResource;
import org.springframework.http.MediaType;
import org.springframework.http.converter.HttpMessageConverter;
import org.springframework.http.server.ServerHttpRequest;
import org.springframework.http.server.ServerHttpResponse;
import org.springframework.http.server.ServletServerHttpRequest;
import org.springframework.web.bind.annotation.ControllerAdvice;
import org.springframework.web.servlet.mvc.method.annotation.ResponseBodyAdvice;

/**
 * Writes the body of an {@code ASYNC} proxy response whose Rust handler set
 * no {@code Content-Type} itself, so the response leaves without one — like
 * {@code SYNC} / {@code DIRECT}, which write straight to the servlet
 * response. Without it Spring's content negotiation would pick a type from
 * the request's {@code Accept} header (a browser's {@code text/html}) while
 * writing the {@code ResponseEntity} body.
 *
 * <p>Only requests the proxy marked ({@link #markProxied}) and only bodies
 * without a {@code Content-Type} are touched; every other response body
 * passes through unchanged. Registered by
 * {@link VesperaBridgeAutoConfiguration}; a hand-wired
 * {@link VesperaProxyController} needs this {@code @ControllerAdvice} bean
 * too, otherwise such ASYNC responses fall back to Spring's negotiated type.
 */
@ControllerAdvice
public final class VesperaAsyncResponseBodyAdvice implements ResponseBodyAdvice<Object> {

    /** Request attribute marking a request the Vespera proxy answers asynchronously. */
    static final String PROXIED_ATTRIBUTE = VesperaAsyncResponseBodyAdvice.class.getName() + ".PROXIED";

    static void markProxied(HttpServletRequest request) {
        request.setAttribute(PROXIED_ATTRIBUTE, Boolean.TRUE);
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
                || response.getHeaders().getContentType() != null
                || !(request instanceof ServletServerHttpRequest servlet)
                || servlet.getServletRequest().getAttribute(PROXIED_ATTRIBUTE) == null) {
            return body;
        }
        try {
            // getBody() commits the entity's status and headers — no Content-Type.
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
