package com.devfive.vespera.bridge;

import java.util.function.Function;

/**
 * Supplies a Rust app's OpenAPI JSON to the springdoc integration
 * ({@link VesperaSpringdocAutoConfiguration}).
 *
 * <p>The autoconfigured default ({@link #inProcess(Function)}) dispatches an
 * in-process {@code GET <spec-path>} to the app through
 * {@link VesperaBridge#dispatchBytes(byte[])} — no HTTP, no socket. Register
 * your own bean to serve the document from elsewhere (e.g. the
 * {@code openapi.json} that {@code vespera!(openapi = ...)} wrote, bundled as
 * a classpath resource).
 */
@FunctionalInterface
public interface VesperaOpenApiSource {

    /**
     * The OpenAPI JSON document of {@code app} ({@code _default} for the
     * default app), or {@code null} when it is not available yet (e.g. the
     * native library is not loaded).
     *
     * @throws Exception when fetching fails (e.g. the spec route answered
     *                   {@code 404}); callers log it and skip the app
     */
    byte[] openApiJson(String app) throws Exception;

    /**
     * In-process source: {@code GET specPathFor.apply(app)} dispatched to the
     * app through the native library.  Answers {@code null} while the native
     * library is not loaded (or was shut down) and throws when the route does
     * not answer {@code 200}.
     */
    static VesperaOpenApiSource inProcess(Function<String, String> specPathFor) {
        return new InProcessOpenApiSource(specPathFor,
                () -> VesperaBridge.isInitialized() && !VesperaBridge.isShutdown(),
                VesperaBridge::dispatchBytes);
    }
}
