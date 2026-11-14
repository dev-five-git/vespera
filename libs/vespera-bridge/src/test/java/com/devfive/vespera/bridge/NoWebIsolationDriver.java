package com.devfive.vespera.bridge;

import java.util.List;
import java.util.concurrent.Callable;
import java.util.concurrent.atomic.AtomicReference;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.ApplicationContextRunner;

/**
 * Runs inside {@link NoWebClasspathTest}'s isolated class loader (no
 * spring-web): starts a non-web context with the core autoconfiguration and
 * checks its beans. Must not reference any spring-web type itself.
 */
public final class NoWebIsolationDriver implements Callable<String> {

    @Override
    public String call() {
        AtomicReference<String> failure = new AtomicReference<>();
        new ApplicationContextRunner()
                .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class))
                .withPropertyValues("vespera.bridge.exclude-patterns=/static/**")
                .run(ctx -> {
                    if (ctx.getStartupFailure() != null) {
                        StringBuilder chain = new StringBuilder();
                        for (Throwable t = ctx.getStartupFailure(); t != null; t = t.getCause()) {
                            chain.append(" | caused by ").append(t);
                        }
                        failure.set(chain.toString());
                        return;
                    }
                    VesperaBridgeProperties props = ctx.getBean(VesperaBridgeProperties.class);
                    if (!List.of("/**").equals(props.getIncludePatterns())) {
                        failure.set("include-patterns default: " + props.getIncludePatterns());
                    } else if (!List.of("/static/**").equals(props.getExcludePatterns())) {
                        failure.set("exclude-patterns: " + props.getExcludePatterns());
                    } else if (ctx.getBeansOfType(VesperaClient.class).size() != 1) {
                        failure.set("missing VesperaClient bean");
                    } else if (ctx.getBeansOfType(VesperaBridgeNativeSettings.class).size() != 1) {
                        failure.set("missing native settings bean");
                    }
                });
        if (failure.get() != null) {
            throw new IllegalStateException("core autoconfiguration: " + failure.get());
        }
        return "ok";
    }
}
