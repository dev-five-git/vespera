package com.devfive.vespera.bridge;

import static org.assertj.core.api.Assertions.assertThat;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;

import java.lang.reflect.Field;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.ApplicationContextRunner;
import org.springframework.boot.test.context.runner.ReactiveWebApplicationContextRunner;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;

/**
 * {@link VesperaBridgeCoreAutoConfiguration}: the native settings, shutdown
 * hook and {@link VesperaClient} exist in a non-web application too, and
 * exactly once next to the servlet / WebFlux proxy.
 */
class VesperaBridgeCoreAutoConfigurationTest {

    private static final AutoConfigurations ALL = AutoConfigurations.of(
            VesperaBridgeCoreAutoConfiguration.class,
            VesperaBridgeAutoConfiguration.class,
            VesperaBridgeReactiveAutoConfiguration.class);

    @AfterEach
    void clearPending() {
        VesperaNativeConfig.RUNTIME_WORKER_THREADS.clearPendingForTest();
        VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.clearPendingForTest();
    }

    private static Long pending(VesperaNativeConfig.Knob knob) throws Exception {
        Field pending = VesperaNativeConfig.Knob.class.getDeclaredField("pending");
        pending.setAccessible(true);
        return (Long) pending.get(knob);
    }

    @Test
    void nonWebApplicationGetsTheClientAndTheNativeSettings() {
        new ApplicationContextRunner()
                .withConfiguration(ALL)
                .withPropertyValues("vespera.bridge.native.worker-threads=3",
                        "vespera.bridge.shutdown-on-close=true")
                .run(ctx -> {
                    assertThat(ctx).hasNotFailed();
                    assertThat(ctx).hasSingleBean(VesperaClient.class);
                    assertThat(ctx).hasSingleBean(VesperaBridgeNativeSettings.class);
                    assertThat(ctx).hasBean("vesperaClient");
                    assertThat(ctx).hasBean("vesperaBridgeNativeSettings");
                    assertThat(ctx).hasBean("vesperaBridgeShutdown");
                    assertEquals(3L, pending(VesperaNativeConfig.RUNTIME_WORKER_THREADS),
                            "vespera.bridge.native.* forwarded without a web stack");
                    // No proxy in a non-web application.
                    assertThat(ctx).doesNotHaveBean(VesperaProxyController.class);
                    assertThat(ctx).doesNotHaveBean(VesperaReactiveProxyHandler.class);
                });
    }

    @Test
    void servletApplicationRegistersTheCoreBeansOnce() {
        new WebApplicationContextRunner()
                .withConfiguration(ALL)
                .run(ctx -> {
                    assertThat(ctx).hasNotFailed();
                    assertThat(ctx).hasSingleBean(VesperaClient.class);
                    assertThat(ctx).hasSingleBean(VesperaBridgeNativeSettings.class);
                    assertThat(ctx).hasSingleBean(VesperaProxyController.class);
                });
    }

    @Test
    void reactiveApplicationRegistersTheCoreBeansOnce() {
        new ReactiveWebApplicationContextRunner()
                .withConfiguration(ALL)
                .run(ctx -> {
                    assertThat(ctx).hasNotFailed();
                    assertThat(ctx).hasSingleBean(VesperaClient.class);
                    assertThat(ctx).hasSingleBean(VesperaBridgeNativeSettings.class);
                    assertThat(ctx).hasSingleBean(VesperaReactiveProxyHandler.class);
                });
    }

    @Test
    void userBeansWin() {
        VesperaClient client = VesperaClient.create().forApp("admin");
        VesperaBridgeNativeSettings settings = new VesperaBridgeNativeSettings();
        new ApplicationContextRunner()
                .withConfiguration(ALL)
                .withBean(VesperaClient.class, () -> client)
                .withBean(VesperaBridgeNativeSettings.class, () -> settings)
                .run(ctx -> {
                    assertSame(client, ctx.getBean(VesperaClient.class));
                    assertSame(settings, ctx.getBean(VesperaBridgeNativeSettings.class));
                });
    }
}
