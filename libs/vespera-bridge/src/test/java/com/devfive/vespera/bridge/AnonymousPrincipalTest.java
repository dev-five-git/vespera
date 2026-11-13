package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.Principal;
import java.time.Duration;
import java.util.List;
import javax.tools.JavaCompiler;
import javax.tools.ToolProvider;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import reactor.core.publisher.Mono;

/**
 * The anonymous-principal check must fail closed: in a native image without
 * a reflection hint (or behind a class loader that cannot see Spring
 * Security) the token class cannot be loaded by name, and the reactive
 * resolver must still skip Spring Security's {@code "anonymousUser"} instead
 * of forwarding it as an authenticated principal.
 *
 * <p>A stand-in {@code AnonymousAuthenticationToken} (and a subclass) is
 * compiled at test time into a class loader the bridge cannot see, so
 * {@code Class.forName} from the bridge fails exactly like it would there.
 */
class AnonymousPrincipalTest {

    @TempDir
    static Path classes;

    private static URLClassLoader tokens;

    @BeforeAll
    static void compileTokenStandIns() throws Exception {
        Path src = classes.resolve("src");
        Path token = src.resolve("org/springframework/security/authentication/AnonymousAuthenticationToken.java");
        Path custom = src.resolve("demo/CustomAnonymousToken.java");
        Files.createDirectories(token.getParent());
        Files.createDirectories(custom.getParent());
        Files.writeString(token, """
                package org.springframework.security.authentication;
                public class AnonymousAuthenticationToken implements java.security.Principal {
                    public String getName() { return "anonymousUser"; }
                }
                """, StandardCharsets.UTF_8);
        Files.writeString(custom, """
                package demo;
                public class CustomAnonymousToken
                        extends org.springframework.security.authentication.AnonymousAuthenticationToken {}
                """, StandardCharsets.UTF_8);
        Path out = classes.resolve("out");
        Files.createDirectories(out);
        JavaCompiler javac = ToolProvider.getSystemJavaCompiler();
        assertNotNull(javac, "tests need a JDK");
        assertEquals(0, javac.run(null, null, null, "-d", out.toString(), token.toString(), custom.toString()));
        tokens = new URLClassLoader(new java.net.URL[] {out.toUri().toURL()},
                AnonymousPrincipalTest.class.getClassLoader());
    }

    @AfterAll
    static void closeLoader() throws Exception {
        tokens.close();
    }

    private static Object instance(String className) throws Exception {
        return Class.forName(className, true, tokens).getDeclaredConstructor().newInstance();
    }

    @Test
    void theTokenClassIsNotLoadableFromTheBridge() {
        // Precondition: the bridge's own lookup fails, as in a hint-less native image.
        assertNull(SecurityAuthorities.load(SecurityAuthorities.ANONYMOUS_TOKEN));
    }

    @Test
    void anonymousTokenIsRecognisedByNameAlongTheSuperclassChain() throws Exception {
        assertTrue(SecurityAuthorities.isAnonymous(instance(SecurityAuthorities.ANONYMOUS_TOKEN)));
        assertTrue(SecurityAuthorities.isAnonymous(instance("demo.CustomAnonymousToken")));
        assertFalse(SecurityAuthorities.isAnonymous((Principal) () -> "alice"));
        assertFalse(SecurityAuthorities.isAnonymous("anonymousUser"));
        assertFalse(SecurityAuthorities.isAnonymous(null));
    }

    @Test
    void reactiveResolverForwardsNoAnonymousPrincipal() throws Exception {
        ExchangePrincipalResolver resolver = new ExchangePrincipalResolver();
        for (String type : List.of(SecurityAuthorities.ANONYMOUS_TOKEN, "demo.CustomAnonymousToken")) {
            Principal anonymous = (Principal) instance(type);
            MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.get("/me"));
            assertNull(resolver.resolvePrincipal(exchange.mutate().principal(Mono.just(anonymous)).build())
                    .block(Duration.ofSeconds(5)), type + " must not be forwarded");
        }
        Principal alice = () -> "alice";
        MockServerWebExchange exchange = MockServerWebExchange.from(MockServerHttpRequest.get("/me"));
        assertEquals(VesperaPrincipal.of("alice"),
                resolver.resolvePrincipal(exchange.mutate().principal(Mono.just(alice)).build())
                        .block(Duration.ofSeconds(5)));
    }
}
