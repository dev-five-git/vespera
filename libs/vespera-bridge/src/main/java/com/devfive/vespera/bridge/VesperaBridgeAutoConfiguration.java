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
 *   <li><strong>Custom dispatch mode policy</strong>:
 *       register a {@code @Bean DispatchModeResolver} —
 *       the default
 *       {@link BidirectionalStreamingDispatchModeResolver} is
 *       automatically disabled.</li>
 *   <li><strong>Completely BYO controller</strong>:
 *       set {@code vespera.bridge.controller-enabled=false} and
 *       provide your own {@code @RestController} that calls the
 *       {@link VesperaBridge} native methods directly.</li>
 * </ul>
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

    @Bean
    @ConditionalOnMissingBean
    public DispatchModeResolver vesperaBridgeDispatchModeResolver() {
        return new BidirectionalStreamingDispatchModeResolver();
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
