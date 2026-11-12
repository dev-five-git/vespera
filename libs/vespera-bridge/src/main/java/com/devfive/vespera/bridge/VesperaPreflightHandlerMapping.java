package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import org.springframework.core.Ordered;
import org.springframework.web.cors.CorsUtils;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.HandlerExecutionChain;
import org.springframework.web.servlet.HandlerMapping;

import java.lang.reflect.Method;
import java.util.Objects;

/**
 * Routes CORS preflight requests straight to {@link VesperaProxyController}
 * ({@code vespera.bridge.cors=rust}, the default).
 *
 * <p>Every {@code AbstractHandlerMapping} — including the one serving the
 * proxy's {@code @RequestMapping("/**")} — replaces the handler of a preflight
 * with Spring's own preflight handler, so the Rust router never sees it.  This
 * mapping implements {@link HandlerMapping} directly (no CORS interception),
 * runs at {@link Ordered#HIGHEST_PRECEDENCE}, matches only preflights and
 * hands them to the proxy method through the regular
 * {@code RequestMappingHandlerAdapter}, so every {@link DispatchMode} works.
 * Preflights outside {@code vespera.bridge.include-patterns} /
 * {@code exclude-patterns} ({@link ProxyPathPatterns}) are left to Spring.
 */
public final class VesperaPreflightHandlerMapping implements HandlerMapping, Ordered {

    private final HandlerMethod handler;
    private final ProxyPathPatterns patterns;

    public VesperaPreflightHandlerMapping(VesperaProxyController controller) {
        this(controller, ProxyPathPatterns.defaults());
    }

    /** Routes only the preflights whose path the proxy owns per {@code patterns}. */
    public VesperaPreflightHandlerMapping(VesperaProxyController controller, ProxyPathPatterns patterns) {
        Objects.requireNonNull(controller, "controller");
        this.handler = new HandlerMethod(controller, proxyMethod());
        this.patterns = Objects.requireNonNull(patterns, "patterns");
    }

    static Method proxyMethod() {
        try {
            return VesperaProxyController.class.getMethod(
                    "proxy", HttpServletRequest.class, HttpServletResponse.class);
        } catch (NoSuchMethodException impossible) {
            throw new IllegalStateException(impossible);
        }
    }

    @Override
    public HandlerExecutionChain getHandler(HttpServletRequest request) {
        return CorsUtils.isPreFlightRequest(request) && patterns.matches(request) ? new HandlerExecutionChain(handler) : null;
    }

    @Override
    public int getOrder() {
        return Ordered.HIGHEST_PRECEDENCE;
    }
}
