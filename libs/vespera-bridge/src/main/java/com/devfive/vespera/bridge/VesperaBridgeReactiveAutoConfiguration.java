package com.devfive.vespera.bridge;

import org.springframework.beans.factory.DisposableBean;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.boot.autoconfigure.condition.ConditionalOnWebApplication;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.context.annotation.Bean;

/**
 * Spring Boot autoconfiguration of the WebFlux proxy — the reactive
 * counterpart of {@link VesperaBridgeAutoConfiguration}, active only in a
 * {@code REACTIVE} web application with Spring WebFlux and Reactor on the
 * classpath (both provided by the application; the bridge never exports
 * them). Every bean is {@code @ConditionalOnMissingBean}.
 *
 * <ul>
 *   <li>{@link VesperaReactiveProxyHandler} (disable with
 *       {@code vespera.bridge.controller-enabled=false}) mapped by
 *       {@link VesperaReactiveHandlerMapping} to
 *       {@code vespera.bridge.include-patterns} minus
 *       {@code exclude-patterns};</li>
 *   <li>{@link ReactiveAppNameResolver} — {@link ReactiveHeaderAppNameResolver}
 *       on {@code vespera.bridge.app-header} restricted by
 *       {@code vespera.bridge.exposed-apps};</li>
 *   <li>{@link ReactivePrincipalResolver} — {@link ExchangePrincipalResolver}
 *       ({@code ServerWebExchange#getPrincipal()}, i.e. the Spring Security
 *       WebFlux authentication), or none with
 *       {@code vespera.bridge.forward-principal=false};</li>
 *   <li>{@link VesperaReactivePreflightFilter} with
 *       {@code vespera.bridge.cors=rust} (default);</li>
 *   <li>the same native settings ({@code vespera.bridge.native.*},
 *       {@code library-name}, {@code shutdown-on-close}) and
 *       {@link VesperaClient} beans as the servlet configuration.</li>
 * </ul>
 *
 * <p>The servlet-only properties {@code dispatch-mode},
 * {@code direct-retry-on-overflow}, {@code async-pool-size} and
 * {@code clear-threadlocals-after-request} do not apply: the reactive proxy
 * always dispatches through the non-blocking async symbol (response
 * streaming for {@code text/event-stream} / NDJSON).
 */
// Class-name guards: skipped cleanly when WebFlux / Reactor are absent.
@AutoConfiguration
@ConditionalOnClass(name = {
        "org.springframework.web.reactive.DispatcherHandler",
        "reactor.core.publisher.Mono"})
@ConditionalOnWebApplication(type = ConditionalOnWebApplication.Type.REACTIVE)
@EnableConfigurationProperties(VesperaBridgeProperties.class)
public class VesperaBridgeReactiveAutoConfiguration {

    /** As the servlet configuration: forwards {@code vespera.bridge.native.*} before the first dispatch. */
    @Bean
    @ConditionalOnMissingBean
    public VesperaBridgeNativeSettings vesperaBridgeNativeSettings(VesperaBridgeProperties props) {
        return VesperaBridgeNativeSettings.apply(props);
    }

    /** {@code vespera.bridge.shutdown-on-close=true}: as the servlet configuration. */
    @Bean
    @ConditionalOnProperty(prefix = "vespera.bridge", name = "shutdown-on-close", havingValue = "true")
    public DisposableBean vesperaBridgeShutdown(
            VesperaBridgeNativeSettings nativeSettings, VesperaBridgeProperties props) {
        return () -> {
            if (nativeSettings.autoInitialized()) {
                VesperaBridge.shutdown(props.getShutdownTimeout());
            }
        };
    }

    /**
     * Programmatic in-process client. Its {@code send()} blocks: from reactive
     * code use {@code sendAsync()} (a {@code CompletableFuture}, e.g. via
     * {@code Mono.fromFuture}) instead.
     */
    @Bean
    @ConditionalOnMissingBean
    public VesperaClient vesperaClient(VesperaBridgeNativeSettings nativeSettings) {
        return VesperaClient.create();
    }

    @Bean
    @ConditionalOnMissingBean
    public ReactiveAppNameResolver vesperaBridgeReactiveAppNameResolver(VesperaBridgeProperties props) {
        return new ReactiveHeaderAppNameResolver(props.getAppHeader(), props.getExposedApps());
    }

    @Bean
    @ConditionalOnMissingBean
    public ReactivePrincipalResolver vesperaBridgeReactivePrincipalResolver(VesperaBridgeProperties props) {
        return props.isForwardPrincipal() ? new ExchangePrincipalResolver() : ReactivePrincipalResolver.none();
    }

    @Bean
    @ConditionalOnProperty(
            prefix = "vespera.bridge",
            name = "controller-enabled",
            havingValue = "true",
            matchIfMissing = true)
    @ConditionalOnMissingBean
    public VesperaReactiveProxyHandler vesperaReactiveProxyHandler(
            ReactiveAppNameResolver appResolver,
            ReactivePrincipalResolver principalResolver,
            VesperaBridgeProperties props,
            VesperaBridgeNativeSettings nativeSettings,
            ObjectProvider<DispatchObserver> observers) {
        return new VesperaReactiveProxyHandler(
                appResolver,
                principalResolver,
                props.getMaxBufferedRequestBytes(),
                props.getMaxBufferedResponseBytes(),
                props.getRequestId().effectiveHeader(),
                observers.orderedStream().toList(),
                props.getCors());
    }

    /** Routes the proxied URL space ({@code include-patterns} / {@code exclude-patterns}) to the handler. */
    @Bean
    @ConditionalOnBean(VesperaReactiveProxyHandler.class)
    @ConditionalOnMissingBean
    public VesperaReactiveHandlerMapping vesperaReactiveHandlerMapping(
            VesperaReactiveProxyHandler handler, VesperaBridgeProperties props) {
        return new VesperaReactiveHandlerMapping(handler, props.proxyPathPatterns());
    }

    /** {@code vespera.bridge.cors=rust} (default): Rust answers CORS preflights. */
    @Bean
    @ConditionalOnBean(VesperaReactiveProxyHandler.class)
    @ConditionalOnProperty(prefix = "vespera.bridge", name = "cors", havingValue = "rust",
            matchIfMissing = true)
    @ConditionalOnMissingBean
    public VesperaReactivePreflightFilter vesperaReactivePreflightFilter(
            VesperaReactiveProxyHandler handler, VesperaBridgeProperties props) {
        return new VesperaReactivePreflightFilter(handler, props.proxyPathPatterns());
    }

    /** Log once at startup when no native library is loaded yet (the proxy answers 503 until it is). */
    @Bean
    public org.springframework.boot.CommandLineRunner vesperaBridgeStartupCheck() {
        return args -> {
            if (!VesperaBridge.isInitialized()) {
                org.slf4j.LoggerFactory.getLogger(VesperaBridgeReactiveAutoConfiguration.class).warn(
                        "vespera native library is not initialized: the proxy answers 503 until "
                                + "VesperaBridge.init(...) runs. Call it before SpringApplication.run "
                                + "or set vespera.bridge.library-name.");
            }
        };
    }
}
