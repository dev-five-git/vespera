package com.devfive.vespera.bridge;

import java.util.List;
import java.util.function.Predicate;
import org.springframework.aot.hint.ExecutableMode;
import org.springframework.aot.hint.RuntimeHints;
import org.springframework.aot.hint.RuntimeHintsRegistrar;
import org.springframework.aot.hint.TypeReference;
import org.springframework.util.ClassUtils;

/**
 * Spring AOT hints for the principal resolvers' reflective Spring Security
 * access in a GraalVM native image: {@code Authentication#getAuthorities()}
 * and {@code GrantedAuthority#getAuthority()}, which {@link SecurityAuthorities}
 * loads by name and invokes reflectively (registered only when Spring
 * Security is on the classpath).
 *
 * <p>Servlet- and WebFlux-free, so both autoconfigurations import it
 * ({@code @ImportRuntimeHints} on {@link VesperaBridgeAutoConfiguration} for
 * {@link ServletPrincipalResolver} and on
 * {@link VesperaBridgeReactiveAutoConfiguration} for
 * {@link ExchangePrincipalResolver}). The anonymous-token check needs no hint:
 * {@link SecurityAuthorities#isAnonymous} compares class names.
 */
public final class VesperaSecurityRuntimeHints implements RuntimeHintsRegistrar {

    @Override
    public void registerHints(RuntimeHints hints, ClassLoader classLoader) {
        registerHints(hints, type -> ClassUtils.isPresent(type, classLoader));
    }

    /** {@code present} answers whether an optional class is on the classpath. */
    static void registerHints(RuntimeHints hints, Predicate<String> present) {
        registerNoArgMethodIfPresent(hints, present,
                SecurityAuthorities.AUTHENTICATION, "getAuthorities");
        registerNoArgMethodIfPresent(hints, present,
                SecurityAuthorities.GRANTED_AUTHORITY, "getAuthority");
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
