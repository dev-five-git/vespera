package com.devfive.vespera.bridge;

import java.util.List;
import java.util.function.Predicate;
import org.springframework.aot.hint.ExecutableMode;
import org.springframework.aot.hint.RuntimeHints;
import org.springframework.aot.hint.RuntimeHintsRegistrar;
import org.springframework.aot.hint.TypeReference;
import org.springframework.util.ClassUtils;

/**
 * Spring AOT hints for a GraalVM native image (Spring Boot {@code nativeCompile}).
 *
 * <p>Everything framework-agnostic (the classes and methods the Rust side
 * looks up through JNI, the bundled {@code native/<platform>/} libraries,
 * {@code Thread.isVirtual}) ships as static reachability metadata under
 * {@code META-INF/native-image/kr.devfive/vespera-bridge/}, so it also applies
 * to plain {@link VesperaClient} apps without Spring. This registrar only adds
 * the Spring-side reflection that AOT cannot infer:
 * <ul>
 *   <li>{@code VesperaProxyController#proxy}, looked up with
 *       {@code getMethod} by {@link VesperaPreflightHandlerMapping}.</li>
 *   <li>Spring Security's {@code Authentication#getAuthorities()} and
 *       {@code GrantedAuthority#getAuthority()}, which
 *       {@link ServletPrincipalResolver} loads by name and invokes
 *       reflectively (registered only when Spring Security is on the
 *       classpath).</li>
 * </ul>
 *
 * <p>Registered through {@code @ImportRuntimeHints} on
 * {@link VesperaBridgeAutoConfiguration}.
 */
public final class VesperaBridgeRuntimeHints implements RuntimeHintsRegistrar {

    @Override
    public void registerHints(RuntimeHints hints, ClassLoader classLoader) {
        registerHints(hints, type -> ClassUtils.isPresent(type, classLoader));
    }

    /** {@code present} answers whether an optional class is on the classpath. */
    static void registerHints(RuntimeHints hints, Predicate<String> present) {
        hints.reflection().registerMethod(
                VesperaPreflightHandlerMapping.proxyMethod(), ExecutableMode.INVOKE);
        registerNoArgMethodIfPresent(hints, present,
                SecurityAuthorities.AUTHENTICATION, "getAuthorities");
        registerNoArgMethodIfPresent(hints, present,
                SecurityAuthorities.GRANTED_AUTHORITY, "getAuthority");
        if (present.test(SecurityAuthorities.ANONYMOUS_TOKEN)) {
            // Class.forName probe for anonymous authentication.
            hints.reflection().registerType(TypeReference.of(SecurityAuthorities.ANONYMOUS_TOKEN));
        }
    }

    private static void registerNoArgMethodIfPresent(
            RuntimeHints hints, Predicate<String> present, String type, String method) {
        if (!present.test(type)) {
            return;
        }
        hints.reflection().registerType(TypeReference.of(type),
                builder -> builder.withMethod(method, List.of(), ExecutableMode.INVOKE));
    }
}
