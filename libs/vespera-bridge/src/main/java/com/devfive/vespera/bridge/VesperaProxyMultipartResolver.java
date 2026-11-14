package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import org.springframework.beans.BeansException;
import org.springframework.beans.factory.BeanFactory;
import org.springframework.beans.factory.BeanFactoryAware;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.beans.factory.config.BeanPostProcessor;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.multipart.MultipartException;
import org.springframework.web.multipart.MultipartHttpServletRequest;
import org.springframework.web.multipart.MultipartResolver;
import org.springframework.web.servlet.DispatcherServlet;
import org.springframework.web.servlet.HandlerExecutionChain;
import org.springframework.web.servlet.HandlerMapping;

import java.util.List;
import java.util.Objects;
import java.util.function.Predicate;

/**
 * {@link MultipartResolver} decorator that leaves requests the Vespera proxy
 * handles unparsed, so their raw {@code multipart/*} body (and the original
 * {@code Content-Type} with its boundary) reach the Rust {@code Multipart}
 * extractor byte-for-byte.
 *
 * <p>{@code DispatcherServlet} resolves multipart requests <em>before</em>
 * handler lookup: Spring Boot's {@code StandardServletMultipartResolver} calls
 * {@code getParts()}, which drains the servlet stream into temp files and
 * applies {@code spring.servlet.multipart.max-file-size} /
 * {@code max-request-size} — the proxy would then forward an empty body, and
 * files over the (1 MB default) limit never reach it at all.  This decorator
 * answers {@link #isMultipart} {@code false} for proxied requests, so none of
 * that happens there; every other request (a Spring {@code @PostMapping} with
 * {@code MultipartFile}, error pages, ...) is delegated unchanged, limits
 * included.
 *
 * <p>Autoconfigured: {@link Installer} wraps the {@code multipartResolver}
 * bean — Spring Boot's or your own — unless
 * {@code vespera.bridge.multipart-passthrough=false}.
 */
public final class VesperaProxyMultipartResolver implements MultipartResolver {

    private final MultipartResolver delegate;
    private final Predicate<HttpServletRequest> proxied;

    /**
     * @param delegate resolver for every request {@code proxied} rejects
     * @param proxied  whether the Vespera proxy handles a request (its body
     *                 must stay unparsed)
     */
    public VesperaProxyMultipartResolver(MultipartResolver delegate, Predicate<HttpServletRequest> proxied) {
        this.delegate = Objects.requireNonNull(delegate, "delegate");
        this.proxied = Objects.requireNonNull(proxied, "proxied");
    }

    /** The wrapped resolver. */
    public MultipartResolver delegate() {
        return delegate;
    }

    @Override
    public boolean isMultipart(HttpServletRequest request) {
        return delegate.isMultipart(request) && !proxied.test(request);
    }

    @Override
    public MultipartHttpServletRequest resolveMultipart(HttpServletRequest request) throws MultipartException {
        return delegate.resolveMultipart(request);
    }

    @Override
    public void cleanupMultipart(MultipartHttpServletRequest request) {
        delegate.cleanupMultipart(request);
    }

    /**
     * Whether {@code DispatcherServlet} will hand {@code request} to the
     * Vespera proxy: the path is inside the proxied URL space
     * ({@link ProxyPathPatterns}) and the first handler mapping that maps it
     * picks {@code VesperaProxyController#proxy} — a more specific Spring
     * controller (e.g. {@code @PostMapping("/upload")} next to the proxy's
     * {@code /**}) wins and keeps normal multipart handling.  Only evaluated
     * for multipart requests.
     */
    static final class ProxyRequestMatcher implements Predicate<HttpServletRequest> {

        private final ObjectProvider<VesperaProxyPathMappings> pathMappings;
        private final ObjectProvider<HandlerMapping> handlerMappings;
        private volatile List<HandlerMapping> ordered;

        ProxyRequestMatcher(ObjectProvider<VesperaProxyPathMappings> pathMappings,
                            ObjectProvider<HandlerMapping> handlerMappings) {
            this.pathMappings = pathMappings;
            this.handlerMappings = handlerMappings;
        }

        @Override
        public boolean test(HttpServletRequest request) {
            VesperaProxyPathMappings mappings = pathMappings.getIfAvailable();
            if (mappings == null || !mappings.patterns().matches(request)) {
                return false;
            }
            for (HandlerMapping mapping : ordered()) {
                HandlerExecutionChain chain;
                try {
                    chain = mapping.getHandler(request);
                } catch (Exception noMatch) {
                    // e.g. 415 / 405 from a partially matching Spring mapping:
                    // DispatcherServlet's own lookup fails the same way.
                    return false;
                }
                if (chain != null) {
                    return chain.getHandler() instanceof HandlerMethod handler
                            && VesperaProxyPathMappings.isProxyMethod(handler);
                }
            }
            return false;
        }

        /** The handler mappings in {@code DispatcherServlet} order (resolved once, after startup). */
        private List<HandlerMapping> ordered() {
            List<HandlerMapping> list = ordered;
            if (list == null) {
                list = handlerMappings.orderedStream().toList();
                ordered = list;
            }
            return list;
        }
    }

    /**
     * Bean post-processor wrapping the {@code multipartResolver} bean (the one
     * {@code DispatcherServlet} uses) in a {@link VesperaProxyMultipartResolver}.
     */
    public static final class Installer implements BeanPostProcessor, BeanFactoryAware {

        private BeanFactory beanFactory;

        @Override
        public void setBeanFactory(BeanFactory beanFactory) {
            this.beanFactory = beanFactory;
        }

        @Override
        public Object postProcessAfterInitialization(Object bean, String beanName) throws BeansException {
            if (DispatcherServlet.MULTIPART_RESOLVER_BEAN_NAME.equals(beanName)
                    && bean instanceof MultipartResolver resolver
                    && !(bean instanceof VesperaProxyMultipartResolver)
                    && beanFactory != null) {
                return new VesperaProxyMultipartResolver(resolver, new ProxyRequestMatcher(
                        beanFactory.getBeanProvider(VesperaProxyPathMappings.class),
                        beanFactory.getBeanProvider(HandlerMapping.class)));
            }
            return bean;
        }
    }
}
