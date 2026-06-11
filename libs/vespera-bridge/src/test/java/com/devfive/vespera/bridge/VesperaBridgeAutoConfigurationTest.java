package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

/**
 * Autoconfigure branch tests for the dispatch-mode policy beans.
 *
 * <p>The contract under test: the autoconfigured default stays
 * {@link BidirectionalStreamingDispatchModeResolver} ("safe for any
 * payload size"); {@code vespera.bridge.dispatch-mode=smart} opts in
 * to {@link SmartDispatchModeResolver}; a user-supplied bean always
 * wins over both.
 */
class VesperaBridgeAutoConfigurationTest {

    // withConfiguration (not withUserConfiguration): autoconfigurations
    // must be evaluated AFTER user configs so @ConditionalOnMissingBean
    // sees user-supplied beans — same ordering as a real Boot app.
    private final WebApplicationContextRunner runner =
            new WebApplicationContextRunner()
                    .withConfiguration(AutoConfigurations.of(VesperaBridgeAutoConfiguration.class));

    @Test
    void defaultResolverIsBidirectionalStreaming() {
        runner.run(
                ctx ->
                        assertInstanceOf(
                                BidirectionalStreamingDispatchModeResolver.class,
                                ctx.getBean(DispatchModeResolver.class),
                                "without the property the published default must not change"));
    }

    @Test
    void smartPropertyOptsIntoSmartResolver() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=smart")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        SmartDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class)));
    }

    @Test
    void userBeanWinsOverSmartProperty() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=smart")
                .withUserConfiguration(CustomResolverConfig.class)
                .run(
                        ctx ->
                                assertInstanceOf(
                                        CustomResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "@ConditionalOnMissingBean: user bean must win"));
    }

    @Test
    void controllerDisabledPropertyStillWorks() {
        runner.withPropertyValues("vespera.bridge.controller-enabled=false")
                .run(ctx -> assertTrue(ctx.getBeansOfType(VesperaProxyController.class).isEmpty()));
    }

    static final class CustomResolver implements DispatchModeResolver {
        @Override
        public DispatchMode resolveMode(jakarta.servlet.http.HttpServletRequest request) {
            return DispatchMode.SYNC;
        }
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomResolverConfig {
        @Bean
        DispatchModeResolver customResolver() {
            return new CustomResolver();
        }
    }
}
