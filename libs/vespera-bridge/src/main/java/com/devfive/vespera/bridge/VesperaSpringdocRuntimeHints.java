package com.devfive.vespera.bridge;

import org.springframework.aot.hint.ExecutableMode;
import org.springframework.aot.hint.RuntimeHints;
import org.springframework.aot.hint.RuntimeHintsRegistrar;

/**
 * Spring AOT hints of the springdoc integration for a GraalVM native image.
 *
 * <p>{@link VesperaOpenApiDocsMapping} looks
 * {@code VesperaOpenApiDocsController#openApi(String)} up with
 * {@code getMethod} and registers it programmatically: the method carries no
 * {@code @RequestMapping}, so Spring's controller hint processing does not
 * cover it, and without this hint a native image fails at startup with
 * {@code NoSuchMethodException} (group mode, the default) — and could not
 * invoke the handler either.
 *
 * <p>Registered through {@code @ImportRuntimeHints} on
 * {@link VesperaSpringdocAutoConfiguration}.
 */
public final class VesperaSpringdocRuntimeHints implements RuntimeHintsRegistrar {

    @Override
    public void registerHints(RuntimeHints hints, ClassLoader classLoader) {
        hints.reflection().registerMethod(
                VesperaOpenApiDocsMapping.openApiMethod(), ExecutableMode.INVOKE);
    }
}
