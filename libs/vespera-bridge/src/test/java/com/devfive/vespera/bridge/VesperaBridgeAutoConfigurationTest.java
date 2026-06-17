package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.concurrent.Executor;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

/**
 * Autoconfigure branch tests for the dispatch-mode policy beans.
 *
 * <p>The contract under test (0.2.0 default flip): the autoconfigured
 * default is {@link SmartDispatchModeResolver} (DIRECT/SYNC fast paths
 * for small bounded requests, measured 2.2–3.2 µs vs 24.1 µs);
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} opts out
 * to {@link BidirectionalStreamingDispatchModeResolver} (pre-0.2.0
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
                                "0.2.0: autoconfigured default flipped to SmartDispatchModeResolver"));
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
                                                + " pre-0.2.0 default"));
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

    @Test
    void directRetryOnOverflowDefaultsToTrueAndCanBeDisabled() {
        runner.run(ctx -> assertTrue(ctx.getBean(VesperaBridgeProperties.class).isDirectRetryOnOverflow()));
        runner.withPropertyValues("vespera.bridge.direct-retry-on-overflow=false")
                .run(ctx -> assertFalse(
                        ctx.getBean(VesperaBridgeProperties.class).isDirectRetryOnOverflow()));
    }

    @Test
    void maxBufferedRequestBytesDefaultsToUnlimitedAndCanBeConfigured() {
        runner.run(ctx -> assertEquals(0L,
                ctx.getBean(VesperaBridgeProperties.class).getMaxBufferedRequestBytes()));
        runner.withPropertyValues("vespera.bridge.max-buffered-request-bytes=12345")
                .run(ctx -> assertEquals(12345L,
                        ctx.getBean(VesperaBridgeProperties.class).getMaxBufferedRequestBytes()));
    }

    @Test
    void asyncResponseExecutorBeanIsReplaceableByName() {
        runner.withUserConfiguration(CustomExecutorConfig.class)
                .run(ctx -> assertSame(
                        CustomExecutorConfig.EXECUTOR,
                        ctx.getBean("vesperaBridgeAsyncResponseExecutor", Executor.class)));
    }

    @Test
    void unknownDispatchModeFallsBackToSmart() {
        // Q7: a typo'd dispatch-mode no longer silently changes semantics —
        // it falls back to smart (with a logged warning), not bidirectional.
        runner.withPropertyValues("vespera.bridge.dispatch-mode=not-a-real-mode")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        SmartDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "unrecognized dispatch-mode must fall back to smart"));
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

    @Configuration(proxyBeanMethods = false)
    static class CustomExecutorConfig {
        static final Executor EXECUTOR = Runnable::run;

        @Bean("vesperaBridgeAsyncResponseExecutor")
        Executor vesperaBridgeAsyncResponseExecutor() {
            return EXECUTOR;
        }
    }
}
