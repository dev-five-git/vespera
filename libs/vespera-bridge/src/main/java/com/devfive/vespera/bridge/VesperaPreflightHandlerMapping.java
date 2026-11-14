package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import jakarta.servlet.http.HttpServletResponse;
import org.springframework.beans.factory.BeanFactoryUtils;
import org.springframework.context.ApplicationContext;
import org.springframework.context.ApplicationContextAware;
import org.springframework.core.Ordered;
import org.springframework.core.annotation.AnnotationAwareOrderComparator;
import org.springframework.http.HttpHeaders;
import org.springframework.web.cors.CorsUtils;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.HandlerExecutionChain;
import org.springframework.web.servlet.HandlerMapping;

import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Set;

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
 *
 * <p>Inside the proxied space a preflight goes to Rust only when Rust would
 * also serve the actual request: as a bean (which receives the
 * {@link ApplicationContext}) it asks the application's other
 * {@link HandlerMapping}s, in order, which handler the actual request
 * ({@code Access-Control-Request-Method}, same path) would get. When that is
 * a Java handler (a {@code @RestController} with {@code @CrossOrigin}, a
 * router function, an actuator endpoint …) the preflight is left to Spring,
 * which answers it with that handler's CORS configuration. The lookup runs on
 * a view of the request whose attribute writes stay private, so it leaves no
 * trace on the real request. Constructed outside a context it routes every
 * owned preflight to Rust.
 */
public final class VesperaPreflightHandlerMapping implements HandlerMapping, Ordered, ApplicationContextAware {

    private final HandlerMethod handler;
    private final ProxyPathPatterns patterns;
    private ApplicationContext context;
    private volatile List<HandlerMapping> others;

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
    public void setApplicationContext(ApplicationContext applicationContext) {
        this.context = applicationContext;
    }

    @Override
    public HandlerExecutionChain getHandler(HttpServletRequest request) {
        if (!CorsUtils.isPreFlightRequest(request) || !patterns.matches(request)) {
            return null;
        }
        return javaHandlesActualRequest(request) ? null : new HandlerExecutionChain(handler);
    }

    /**
     * Whether the first other handler mapping that maps the actual request
     * hands it to something other than the Vespera proxy.
     */
    private boolean javaHandlesActualRequest(HttpServletRequest preflight) {
        List<HandlerMapping> mappings = otherMappings();
        if (mappings.isEmpty()) {
            return false;
        }
        String method = preflight.getHeader(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD).trim();
        ActualRequestView actual = new ActualRequestView(preflight, method);
        for (HandlerMapping mapping : mappings) {
            HandlerExecutionChain chain;
            try {
                chain = mapping.getHandler(actual);
            } catch (Exception springOwned) {
                // A mapping that claims the path but rejects the request
                // (405, 406, 415 …): Spring answers the actual request.
                return true;
            }
            if (chain != null) {
                return !isProxy(chain.getHandler());
            }
        }
        return false;
    }

    private static boolean isProxy(Object handler) {
        return handler instanceof HandlerMethod method
                && VesperaProxyController.class.isAssignableFrom(method.getBeanType());
    }

    private List<HandlerMapping> otherMappings() {
        List<HandlerMapping> resolved = others;
        if (resolved == null) {
            ApplicationContext ctx = context;
            if (ctx == null) {
                return List.of();
            }
            List<HandlerMapping> found = new ArrayList<>();
            for (HandlerMapping mapping : BeanFactoryUtils.beansOfTypeIncludingAncestors(
                    ctx, HandlerMapping.class, true, false).values()) {
                if (mapping != this) {
                    found.add(mapping);
                }
            }
            AnnotationAwareOrderComparator.sort(found);
            resolved = List.copyOf(found);
            others = resolved;
        }
        return resolved;
    }

    @Override
    public int getOrder() {
        return Ordered.HIGHEST_PRECEDENCE;
    }

    /**
     * The actual request a preflight announces (its
     * {@code Access-Control-Request-Method}); request attributes the
     * handler-mapping lookup writes stay local to this view.
     */
    static final class ActualRequestView extends HttpServletRequestWrapper {
        private final String method;
        private final Map<String, Object> attributes = new HashMap<>();
        private final Set<String> removed = new HashSet<>();

        ActualRequestView(HttpServletRequest request, String method) {
            super(request);
            this.method = method;
        }

        @Override
        public String getMethod() {
            return method;
        }

        @Override
        public Object getAttribute(String name) {
            if (attributes.containsKey(name)) {
                return attributes.get(name);
            }
            return removed.contains(name) ? null : super.getAttribute(name);
        }

        @Override
        public java.util.Enumeration<String> getAttributeNames() {
            Set<String> names = new LinkedHashSet<>(Collections.list(super.getAttributeNames()));
            names.removeAll(removed);
            names.addAll(attributes.keySet());
            return Collections.enumeration(names);
        }

        @Override
        public void setAttribute(String name, Object value) {
            if (value == null) {
                removeAttribute(name);
                return;
            }
            attributes.put(name, value);
            removed.remove(name);
        }

        @Override
        public void removeAttribute(String name) {
            attributes.remove(name);
            removed.add(name);
        }
    }
}
