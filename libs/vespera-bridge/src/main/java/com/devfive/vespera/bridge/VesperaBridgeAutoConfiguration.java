package com.devfive.vespera.bridge;

import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.boot.autoconfigure.condition.ConditionalOnWebApplication;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.annotation.Bean;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.beans.factory.annotation.Qualifier;

import jakarta.servlet.Filter;
import jakarta.servlet.FilterChain;
import jakarta.servlet.ServletRequest;
import jakarta.servlet.ServletResponse;
import java.util.concurrent.Executor;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

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
 *   <li><strong>Exposed apps (security)</strong>:
 *       set {@code vespera.bridge.exposed-apps} — the app header is
 *       client-controlled, so restrict which apps it may select.</li>
 *   <li><strong>Custom app selection</strong>:
 *       register a {@code @Bean AppNameResolver} —
 *       the default {@link HeaderAppNameResolver} is automatically
 *       disabled.</li>
 *   <li><strong>Conservative dispatch mode (opt-out from smart)</strong>:
 *       set {@code vespera.bridge.dispatch-mode=bidirectional-streaming}
 *       to restore the pre-0.2.0 default
 *       ({@link BidirectionalStreamingDispatchModeResolver}) — every
 *       request that may carry a body streams both ways. Use when
 *       you want maximally uniform handler invocation semantics and
 *       are willing to pay the ~24 µs/request streaming cost on
 *       small JSON-RPC payloads.</li>
 *   <li><strong>Custom dispatch mode policy</strong>:
 *       register a {@code @Bean DispatchModeResolver} —
 *       the default {@link SmartDispatchModeResolver} is
 *       automatically disabled.</li>
 *   <li><strong>Proxied URL space</strong>:
 *       set {@code vespera.bridge.include-patterns} /
 *       {@code vespera.bridge.exclude-patterns} — excluded paths fall
 *       through to other controllers and static resources.</li>
 *   <li><strong>Programmatic calls</strong>: inject the
 *       {@link VesperaClient} bean (replaceable) to call the Rust app
 *       from services, jobs or listeners without HTTP.</li>
 *   <li><strong>Completely BYO controller</strong>:
 *       set {@code vespera.bridge.controller-enabled=false} and
 *       provide your own {@code @RestController} that calls the
 *       {@link VesperaBridge} native methods directly.</li>
   *   <li><strong>Async response continuation executor</strong>:
   *       replace the {@code vesperaBridgeAsyncResponseExecutor} bean.
   *       The default is a small named daemon-thread pool.</li>
 * </ul>
 *
 * <p><strong>0.2.0 behavior change:</strong> the autoconfigured
 * default {@link DispatchModeResolver} flipped from
 * {@link BidirectionalStreamingDispatchModeResolver} to
 * {@link SmartDispatchModeResolver}. Measured on a small {@code GET
 * /health} round-trip through the real JNI boundary: DIRECT 2.2 µs /
 * SYNC 3.2 µs vs the old bidirectional 24.1 µs. Restore the old
 * behavior with {@code vespera.bridge.dispatch-mode=bidirectional-streaming}.
 */
// Class-name (not Class-literal) guard: evaluated from bytecode metadata, so this
// configuration is skipped cleanly when Spring MVC or the servlet API is absent
// (Spring is a provided dependency since the bridge stopped exporting the
// starter), instead of failing on a missing class.
@AutoConfiguration
@ConditionalOnClass(name = {
        "jakarta.servlet.Servlet",
        "org.springframework.web.servlet.DispatcherServlet"})
@ConditionalOnWebApplication(type = ConditionalOnWebApplication.Type.SERVLET)
@EnableConfigurationProperties(VesperaBridgeProperties.class)
public class VesperaBridgeAutoConfiguration {

    /**
     * Forwards {@code vespera.bridge.native.*} to the {@link VesperaBridge}
     * {@code configure*} methods during context startup, before the proxy
     * serves its first request (the controller bean depends on it), so the
     * values land before the first dispatch fixes the native runtime/limits.
     */
    @Bean
    @ConditionalOnMissingBean
    public VesperaBridgeNativeSettings vesperaBridgeNativeSettings(VesperaBridgeProperties props) {
        props.getNative().apply(props.getMaxBufferedResponseBytes());
        return new VesperaBridgeNativeSettings(autoInit(props.getLibraryName()));
    }

    /**
     * {@code vespera.bridge.library-name}: initialise the native library
     * unless the application already did. Returns whether this call loaded it.
     */
    static boolean autoInit(String libraryName) {
        if (libraryName == null || libraryName.isBlank()) {
            return false;
        }
        synchronized (VesperaBridge.class) {
            if (VesperaBridge.isInitialized()) {
                return false;
            }
            VesperaBridge.init(libraryName.trim());
            return true;
        }
    }

    /**
     * {@code vespera.bridge.shutdown-on-close=true}: shut the native runtime
     * down when the context closes — only when the autoconfiguration
     * initialised the library itself (an application-initialised library may
     * outlive this context). Destroyed after the web server stopped serving.
     */
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
     * Programmatic in-process client for the default app
     * ({@code client.forApp("admin")} for a named one). Independent of
     * {@code vespera.bridge.controller-enabled}; depends on the native
     * settings bean so {@code vespera.bridge.native.*} and
     * {@code library-name} are applied before the first client dispatch.
     */
    @Bean
    @ConditionalOnMissingBean
    public VesperaClient vesperaClient(VesperaBridgeNativeSettings nativeSettings) {
        return VesperaClient.create();
    }

    @Bean
    @ConditionalOnMissingBean
    public AppNameResolver vesperaBridgeAppNameResolver(VesperaBridgeProperties props) {
        return new HeaderAppNameResolver(props.getAppHeader(), props.getExposedApps());
    }

    /**
     * Opt-out conservative dispatch mode: every request that may
     * carry a body streams both ways
     * ({@link BidirectionalStreamingDispatchModeResolver}). Restores
     * the pre-0.2.0 default.
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
     * Autoconfigured default since 0.2.0:
     * {@link SmartDispatchModeResolver} picks per request — DIRECT
     * (pooled direct buffers, no JNI array copies) for small/bodyless
     * safe requests, SYNC for small unsafe requests,
     * BIDIRECTIONAL_STREAMING for everything else.
     *
     * <p>The two trade-offs callers accept on the new default:
     * <ul>
     *   <li>DIRECT retries (re-runs the Rust handler) once when a
     *       response exceeds {@code vespera.direct.maxBufferBytes}
     *       (default 4 MiB). This is why DIRECT is restricted to safe
     *       methods (GET/HEAD/OPTIONS).</li>
     *   <li>SYNC buffers the full response on the JVM heap. The
     *       256 KiB request-size gate keeps the response size
     *       reasonable for JSON-RPC-shaped traffic.</li>
     * </ul>
     *
     * <p>Restore the pre-0.2.0 behavior with
     * {@code vespera.bridge.dispatch-mode=bidirectional-streaming}.
     */
    @Bean
    @ConditionalOnMissingBean
    public DispatchModeResolver vesperaBridgeDispatchModeResolver(VesperaBridgeProperties props) {
        // This default bean is created for `dispatch-mode=smart` AND for any
        // unrecognized value (the `bidirectional-streaming` opt-out has its own
        // @ConditionalOnProperty bean above). Surface a typo instead of letting
        // it silently change dispatch semantics to smart.
        String mode = props.getDispatchMode();
        if (mode != null
                && !mode.equalsIgnoreCase("smart")
                && !mode.equalsIgnoreCase("bidirectional-streaming")) {
            throw new IllegalArgumentException(
                    "Unrecognized vespera.bridge.dispatch-mode '" + mode
                            + "'. Valid values: 'smart' (default), 'bidirectional-streaming'.");
        }
        return new SmartDispatchModeResolver();
    }

    @Bean("vesperaBridgeAsyncResponseExecutor")
    @ConditionalOnMissingBean(name = "vesperaBridgeAsyncResponseExecutor")
    public ExecutorService vesperaBridgeAsyncResponseExecutor(VesperaBridgeProperties props) {
        // Default (asyncPoolSize <= 0) preserves the historical sizing:
        // Math.max(2, Math.min(4, cpus)). A positive vespera.bridge.async-pool-size
        // overrides the cap for high-concurrency async dispatch (clamped to >= 1).
        int configured = props.getAsyncPoolSize();
        int threads = configured > 0
                ? configured
                : Math.max(2, Math.min(4, Runtime.getRuntime().availableProcessors()));
        AtomicInteger seq = new AtomicInteger(1);
        ThreadFactory factory = task -> {
            Thread thread = new Thread(task, "vespera-bridge-async-response-" + seq.getAndIncrement());
            thread.setDaemon(true);
            return thread;
        };
        int queueCapacity = Math.max(256, threads * 256);
        return new ThreadPoolExecutor(
                threads,
                threads,
                0L,
                TimeUnit.MILLISECONDS,
                new LinkedBlockingQueue<>(queueCapacity),
                factory,
                // AbortPolicy, NOT CallerRunsPolicy: this executor's tasks are
                // submitted from the thread that completes the native dispatch
                // future — a Rust Tokio worker. CallerRunsPolicy would run the
                // heavy wire-response build on that Tokio worker under
                // saturation, stealing native dispatch capacity (violating the
                // documented "no heavy continuations on Tokio workers"
                // contract). AbortPolicy rejects instead; the proxy's
                // dispatchAsyncFlow maps the rejection to a 503 backpressure
                // signal. The bounded queue still absorbs bursts first.
                new ThreadPoolExecutor.AbortPolicy());
    }

    @Bean
    @ConditionalOnMissingBean
    public VesperaBridgeThreadLocalCleanup vesperaBridgeThreadLocalCleanup() {
        return new VesperaBridgeThreadLocalCleanup();
    }

    @Bean
    @ConditionalOnProperty(
            prefix = "vespera.bridge",
            name = "clear-threadlocals-after-request",
            havingValue = "true")
    public FilterRegistrationBean<Filter> vesperaBridgeThreadLocalCleanupFilter() {
        FilterRegistrationBean<Filter> registration = new FilterRegistrationBean<>();
        registration.setFilter((ServletRequest request, ServletResponse response, FilterChain chain) -> {
            try {
                chain.doFilter(request, response);
            } finally {
                VesperaBridge.clearCurrentThreadBuffers();
            }
        });
        registration.setName("vesperaBridgeThreadLocalCleanupFilter");
        registration.setOrder(Integer.MAX_VALUE);
        return registration;
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
            DispatchModeResolver modeResolver,
            @Qualifier("vesperaBridgeAsyncResponseExecutor") Executor asyncResponseExecutor,
            VesperaBridgeProperties props,
            VesperaBridgeNativeSettings nativeSettings,
            ObjectProvider<DispatchObserver> observers) {
        return new VesperaProxyController(
                appResolver,
                modeResolver,
                asyncResponseExecutor,
                props.isDirectRetryOnOverflow(),
                props.getMaxBufferedRequestBytes(),
                props.getMaxBufferedResponseBytes(),
                props.getRequestId().effectiveHeader(),
                observers.orderedStream().toList(),
                props.getCors());
    }

    /**
     * {@code vespera.bridge.cors=rust} (default): route CORS preflights to
     * the proxy controller instead of Spring's built-in preflight handling,
     * so the Rust router (e.g. a {@code CorsLayer}) answers them.
     */
    @Bean
    @ConditionalOnBean(VesperaProxyController.class)
    @ConditionalOnProperty(prefix = "vespera.bridge", name = "cors", havingValue = "rust",
            matchIfMissing = true)
    @ConditionalOnMissingBean
    public VesperaPreflightHandlerMapping vesperaPreflightHandlerMapping(
            VesperaProxyController controller, VesperaBridgeProperties props) {
        return new VesperaPreflightHandlerMapping(controller, props.proxyPathPatterns());
    }

    /**
     * {@code vespera.bridge.include-patterns} / {@code exclude-patterns}:
     * narrows the proxy's {@code /**} request mapping so excluded paths fall
     * through to the rest of Spring MVC (other handlers, static resources,
     * {@code 404}). A no-op with the defaults. Invalid patterns fail startup.
     */
    @Bean
    @ConditionalOnBean(VesperaProxyController.class)
    @ConditionalOnMissingBean
    public VesperaProxyPathMappings vesperaProxyPathMappings(
            VesperaBridgeProperties props,
            ObjectProvider<org.springframework.web.servlet.mvc.method.annotation.RequestMappingHandlerMapping>
                    handlerMappings) {
        return new VesperaProxyPathMappings(props.proxyPathPatterns(), handlerMappings);
    }

    /**
     * Log once at startup when no native library is loaded yet: until it is,
     * the proxy answers every request {@code 503} (see the
     * {@code vespera} health indicator with Spring Boot Actuator).
     */
    @Bean
    public org.springframework.boot.CommandLineRunner vesperaBridgeStartupCheck() {
        return args -> {
            if (!VesperaBridge.isInitialized()) {
                org.slf4j.LoggerFactory.getLogger(VesperaBridgeAutoConfiguration.class).warn(
                        "vespera native library is not initialized: the proxy answers 503 until "
                                + "VesperaBridge.init(...) runs. Call it before SpringApplication.run "
                                + "or set vespera.bridge.library-name.");
            }
        };
    }
}
