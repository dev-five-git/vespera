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
 * <p>The contract under test (1.0.0 default flip): the autoconfigured
 * default is {@link SmartDispatchModeResolver} (DIRECT/SYNC fast paths
 * for small bounded requests, measured 2.2–3.2 µs vs 24.1 µs);
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} opts out
 * to {@link BidirectionalStreamingDispatchModeResolver} (pre-1.0.0
 * behavior); {@code vespera.bridge.dispatch-mode=smart} explicitly
 * pins the new default; a user-supplied bean always wins over all of
 * the above via {@code @ConditionalOnMissingBean}.
 */
class VesperaBridgeAutoConfigurationTest {

    // withConfiguration (not withUserConfiguration): autoconfigurations
    // must be evaluated AFTER user configs so @ConditionalOnMissingBean
    // sees user-supplied beans — same ordering as a real Boot app.
    private final WebApplicationContextRunner runner =
            new WebApplicationContextRunner()
                    .withConfiguration(AutoConfigurations.of(VesperaBridgeAutoConfiguration.class));

    @Test
    void defaultResolverIsSmart() {
        runner.run(
                ctx ->
                        assertInstanceOf(
                                SmartDispatchModeResolver.class,
                                ctx.getBean(DispatchModeResolver.class),
                                "1.0.0: autoconfigured default flipped to SmartDispatchModeResolver"));
    }

    @Test
    void smartPropertyExplicitlyPinsSmartResolver() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=smart")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        SmartDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "explicit dispatch-mode=smart must keep the new default"));
    }

    @Test
    void bidirectionalStreamingPropertyOptsOutToStreamingResolver() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=bidirectional-streaming")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        BidirectionalStreamingDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "dispatch-mode=bidirectional-streaming must restore the"
                                                + " pre-1.0.0 default"));
    }

    @Test
    void userBeanWinsOverDefault() {
        runner.withUserConfiguration(CustomResolverConfig.class)
                .run(
                        ctx ->
                                assertInstanceOf(
                                        CustomResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "@ConditionalOnMissingBean: user bean must win over the"
                                                + " autoconfigured smart default"));
    }

    @Test
    void userBeanWinsOverBidirectionalStreamingProperty() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=bidirectional-streaming")
                .withUserConfiguration(CustomResolverConfig.class)
                .run(
                        ctx ->
                                assertInstanceOf(
                                        CustomResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "@ConditionalOnMissingBean: user bean must win even when"
                                                + " the opt-out property is set"));
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
