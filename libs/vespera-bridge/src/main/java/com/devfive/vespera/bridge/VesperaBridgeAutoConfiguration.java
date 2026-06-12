package com.devfive.vespera.bridge;

import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.boot.autoconfigure.condition.ConditionalOnWebApplication;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

/**
 * Spring Boot autoconfigure entry point for vespera-bridge.
 *
 * <p>Wires up zero-configuration defaults so a typical Spring Boot
 * app needs only {@link VesperaBridge#init(String)} and the
 * routes published in vespera's {@code openapi.json} are reachable
 * at the same URLs.  Every bean is gated by
 * {@code @ConditionalOnMissingBean}, so any user-supplied custom
 * bean automatically wins.
 *
 * <p>Customization recipes:
 * <ul>
 *   <li><strong>Header name</strong>:
 *       set {@code vespera.bridge.app-header}.</li>
 *   <li><strong>Custom app selection</strong>:
 *       register a {@code @Bean AppNameResolver} —
 *       the default {@link HeaderAppNameResolver} is automatically
 *       disabled.</li>
 *   <li><strong>Conservative dispatch mode (opt-out from smart)</strong>:
 *       set {@code vespera.bridge.dispatch-mode=bidirectional-streaming}
 *       to restore the pre-1.0.0 default
 *       ({@link BidirectionalStreamingDispatchModeResolver}) — every
 *       request that may carry a body streams both ways. Use when
 *       you want maximally uniform handler invocation semantics and
 *       are willing to pay the ~24 µs/request streaming cost on
 *       small JSON-RPC payloads.</li>
 *   <li><strong>Custom dispatch mode policy</strong>:
 *       register a {@code @Bean DispatchModeResolver} —
 *       the default {@link SmartDispatchModeResolver} is
 *       automatically disabled.</li>
 *   <li><strong>Completely BYO controller</strong>:
 *       set {@code vespera.bridge.controller-enabled=false} and
 *       provide your own {@code @RestController} that calls the
 *       {@link VesperaBridge} native methods directly.</li>
 * </ul>
 *
 * <p><strong>1.0.0 behavior change:</strong> the autoconfigured
 * default {@link DispatchModeResolver} flipped from
 * {@link BidirectionalStreamingDispatchModeResolver} to
 * {@link SmartDispatchModeResolver}. Measured on a small {@code GET
 * /health} round-trip through the real JNI boundary: DIRECT 2.2 µs /
 * SYNC 3.2 µs vs the old bidirectional 24.1 µs. Restore the old
 * behavior with {@code vespera.bridge.dispatch-mode=bidirectional-streaming}.
 */
@Configuration(proxyBeanMethods = false)
@ConditionalOnWebApplication(type = ConditionalOnWebApplication.Type.SERVLET)
@EnableConfigurationProperties(VesperaBridgeProperties.class)
public class VesperaBridgeAutoConfiguration {

    @Bean
    @ConditionalOnMissingBean
    public AppNameResolver vesperaBridgeAppNameResolver(VesperaBridgeProperties props) {
        return new HeaderAppNameResolver(props.getAppHeader());
    }

    /**
     * Opt-out conservative dispatch mode: every request that may
     * carry a body streams both ways
     * ({@link BidirectionalStreamingDispatchModeResolver}). Restores
     * the pre-1.0.0 default.
     *
     * <p>Declared <em>before</em> the autoconfigured default so that
     * {@code @ConditionalOnMissingBean} on the default skips when this
     * one is created.  Opt-in via
     * {@code vespera.bridge.dispatch-mode=bidirectional-streaming};
     * the autoconfigured default is now
     * {@link SmartDispatchModeResolver} because DIRECT/SYNC are
     * 7–11× cheaper than streaming for small bounded requests
     * (measured 2.2–3.2 µs vs 24.1 µs on a small {@code GET /health}).
     */
    @Bean
    @ConditionalOnProperty(
            prefix = "vespera.bridge",
            name = "dispatch-mode",
            havingValue = "bidirectional-streaming")
    @ConditionalOnMissingBean
    public DispatchModeResolver vesperaBridgeBidirectionalStreamingDispatchModeResolver() {
        return new BidirectionalStreamingDispatchModeResolver();
    }

    /**
     * Autoconfigured default since 1.0.0:
     * {@link SmartDispatchModeResolver} picks per request — DIRECT
     * (pooled direct buffers, no JNI array copies) for small/bodyless
     * idempotent requests, SYNC for small non-idempotent requests,
     * BIDIRECTIONAL_STREAMING for everything else.
     *
     * <p>The two trade-offs callers accept on the new default:
     * <ul>
     *   <li>DIRECT retries (re-runs the Rust handler) once when a
     *       response exceeds {@code vespera.direct.maxBufferBytes}
     *       (default 4 MiB). This is why DIRECT is restricted to
     *       idempotent methods (GET/HEAD/PUT/DELETE/OPTIONS).</li>
     *   <li>SYNC buffers the full response on the JVM heap. The
     *       256 KiB request-size gate keeps the response size
     *       reasonable for JSON-RPC-shaped traffic.</li>
     * </ul>
     *
     * <p>Restore the pre-1.0.0 behavior with
     * {@code vespera.bridge.dispatch-mode=bidirectional-streaming}.
     */
    @Bean
    @ConditionalOnMissingBean
    public DispatchModeResolver vesperaBridgeDispatchModeResolver() {
        return new SmartDispatchModeResolver();
    }

    @Bean
    @ConditionalOnProperty(
        prefix = "vespera.bridge",
        name = "controller-enabled",
        havingValue = "true",
        matchIfMissing = true)
    @ConditionalOnMissingBean
    public VesperaProxyController vesperaProxyController(
            AppNameResolver appResolver,
            DispatchModeResolver modeResolver) {
        return new VesperaProxyController(appResolver, modeResolver);
    }
}
