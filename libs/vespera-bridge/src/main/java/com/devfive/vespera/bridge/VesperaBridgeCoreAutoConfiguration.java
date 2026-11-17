package com.devfive.vespera.bridge;

import java.time.Duration;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.context.annotation.Bean;

/**
 * Transport-independent part of the vespera-bridge autoconfiguration, active
 * in <em>every</em> Spring Boot application, including non-web ones
 * ({@code spring.main.web-application-type=none}: batch jobs, workers,
 * schedulers):
 *
 * <ul>
 *   <li>{@code vesperaBridgeNativeSettings} — forwards
 *       {@code vespera.bridge.native.*} to the native side and, with
 *       {@code vespera.bridge.library-name}, initialises the library;</li>
 *   <li>{@code vesperaBridgeShutdown} —
 *       {@code vespera.bridge.shutdown-on-close=true};</li>
 *   <li>{@code vesperaClient} — the programmatic in-process
 *       {@link VesperaClient}.</li>
 * </ul>
 *
 * <p>The servlet ({@link VesperaBridgeAutoConfiguration}) and WebFlux
 * ({@link VesperaBridgeReactiveAutoConfiguration}) proxies run after it and
 * depend on the native settings bean, so the settings are applied before the
 * proxy (or the client) dispatches anything. Every bean except
 * {@code vesperaBridgeShutdown} (property-gated only: a
 * {@code DisposableBean} gate would match unrelated beans) is
 * {@code @ConditionalOnMissingBean}.
 */
@AutoConfiguration
@EnableConfigurationProperties(VesperaBridgeProperties.class)
public class VesperaBridgeCoreAutoConfiguration {

    /**
     * Forwards {@code vespera.bridge.native.*} to the {@link VesperaBridge}
     * {@code configure*} methods during context startup, before the proxy
     * serves its first request or the client dispatches (both depend on it),
     * so the values land before the first dispatch fixes the native
     * runtime/limits.
     */
    @Bean
    @ConditionalOnMissingBean
    public VesperaBridgeNativeSettings vesperaBridgeNativeSettings(VesperaBridgeProperties props) {
        return VesperaBridgeNativeSettings.apply(props);
    }

    /**
     * {@code vespera.bridge.shutdown-on-close=true}: shut the native runtime
     * down when the context closes — only when the autoconfiguration
     * initialised the library itself (an application-initialised library may
     * outlive this context). Destroyed after a web server stopped serving.
     */
    @Bean
    @ConditionalOnProperty(prefix = "vespera.bridge", name = "shutdown-on-close", havingValue = "true")
    public DisposableBean vesperaBridgeShutdown(
            VesperaBridgeNativeSettings nativeSettings, VesperaBridgeProperties props) {
        return shutdownOnClose(nativeSettings, props, timeout -> VesperaBridge.shutdown(timeout));
    }

    /** {@link #vesperaBridgeShutdown} with the shutdown action as a test seam. */
    static DisposableBean shutdownOnClose(VesperaBridgeNativeSettings nativeSettings,
            VesperaBridgeProperties props, java.util.function.Consumer<Duration> shutdown) {
        return () -> {
            if (nativeSettings.autoInitialized()) {
                shutdown.accept(props.getShutdownTimeout());
            }
        };
    }

    /**
     * Programmatic in-process client for the default app
     * ({@code client.forApp("admin")} for a named one). Independent of the
     * web stack and of {@code vespera.bridge.controller-enabled}; depends on
     * the native settings bean so {@code vespera.bridge.native.*} and
     * {@code library-name} are applied before the first client dispatch.
     */
    @Bean
    @ConditionalOnMissingBean
    public VesperaClient vesperaClient(VesperaBridgeNativeSettings nativeSettings) {
        return VesperaClient.create();
    }

    /**
     * The not-initialized startup WARN of the servlet and WebFlux
     * autoconfigurations: names the proxy only when one exists
     * ({@code vespera.bridge.controller-enabled=false} leaves only
     * {@link VesperaClient}). Lives here, servlet- and WebFlux-free, so both
     * web stacks share it.
     */
    static String startupWarning(boolean proxy) {
        return "vespera native library is not initialized: "
                + (proxy ? "the proxy answers 503" : "VesperaClient requests fail")
                + " until VesperaBridge.init(...) runs. Call it before SpringApplication.run "
                + "or set vespera.bridge.library-name.";
    }
}
