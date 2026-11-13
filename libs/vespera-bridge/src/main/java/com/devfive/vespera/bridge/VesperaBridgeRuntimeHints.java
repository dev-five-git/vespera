package com.devfive.vespera.bridge;

import org.springframework.aot.hint.ExecutableMode;
import org.springframework.aot.hint.RuntimeHints;
import org.springframework.aot.hint.RuntimeHintsRegistrar;

/**
 * Spring AOT hints of the servlet proxy for a GraalVM native image (Spring
 * Boot {@code nativeCompile}).
 *
 * <p>Everything framework-agnostic (the classes and methods the Rust side
 * looks up through JNI, the bundled {@code native/<platform>/} libraries,
 * {@code Thread.isVirtual}) ships as static reachability metadata under
 * {@code META-INF/native-image/kr.devfive/vespera-bridge/}, so it also applies
 * to plain {@link VesperaClient} apps without Spring. This registrar only adds
 * {@code VesperaProxyController#proxy}, looked up with {@code getMethod} by
 * {@link VesperaPreflightHandlerMapping} (servlet-only). The Spring Security
 * reflection shared with the WebFlux proxy lives in
 * {@link VesperaSecurityRuntimeHints}.
 *
 * <p>Registered through {@code @ImportRuntimeHints} on
 * {@link VesperaBridgeAutoConfiguration}.
 */
public final class VesperaBridgeRuntimeHints implements RuntimeHintsRegistrar {

    @Override
    public void registerHints(RuntimeHints hints, ClassLoader classLoader) {
        hints.reflection().registerMethod(
                VesperaPreflightHandlerMapping.proxyMethod(), ExecutableMode.INVOKE);
    }
}
