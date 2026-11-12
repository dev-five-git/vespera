package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;

/**
 * Spring, the servlet API and slf4j are {@code compileOnly} (provided by the
 * consuming app). The non-Spring surface — {@link VesperaBridge}'s codec and
 * buffer helpers — must therefore load and run on a classpath that contains
 * ONLY the bridge's own classes; a stray reference to a Spring-typed class
 * would surface here as {@link NoClassDefFoundError}.
 */
class NoSpringClasspathTest {

    @Test
    void codecAndBufferCleanupRunWithoutSpringOnTheClasspath() throws Exception {
        URL mainClasses = VesperaBridge.class.getProtectionDomain().getCodeSource().getLocation();
        try (URLClassLoader isolated =
                new URLClassLoader(new URL[] {mainClasses}, ClassLoader.getPlatformClassLoader())) {
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("org.springframework.http.HttpHeaders", false, isolated));
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("jakarta.servlet.http.HttpServletRequest", false, isolated));

            Class<?> bridge = Class.forName("com.devfive.vespera.bridge.VesperaBridge", true, isolated);

            Method encode = bridge.getMethod("encodeRequest",
                    String.class, String.class, String.class, Map.class, byte[].class);
            byte[] wire = (byte[]) encode.invoke(null, "GET", "/health", "",
                    Map.of("accept", "text/plain"), "x".getBytes(StandardCharsets.UTF_8));
            Method encodeHere = VesperaBridge.class.getMethod("encodeRequest",
                    String.class, String.class, String.class, Map.class, byte[].class);
            byte[] expected = (byte[]) encodeHere.invoke(null, "GET", "/health", "",
                    Map.of("accept", "text/plain"), "x".getBytes(StandardCharsets.UTF_8));
            assertEquals(new String(expected, StandardCharsets.ISO_8859_1),
                    new String(wire, StandardCharsets.ISO_8859_1));

            bridge.getMethod("clearCurrentThreadBuffers").invoke(null);
        }
    }

    /**
     * The Kotlin extensions ship in the same JAR with kotlin-stdlib /
     * kotlinx-coroutines as {@code compileOnly}: the Java client API must
     * load and run without them (no Kotlin class reachable from Java code).
     */
    @Test
    void javaClientApiRunsWithoutKotlinOnTheClasspath() throws Exception {
        URL mainClasses = VesperaBridge.class.getProtectionDomain().getCodeSource().getLocation();
        try (URLClassLoader isolated =
                new URLClassLoader(new URL[] {mainClasses}, ClassLoader.getPlatformClassLoader())) {
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("kotlin.Unit", false, isolated));
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("kotlinx.coroutines.future.FutureKt", false, isolated));

            Class<?> client = Class.forName("com.devfive.vespera.bridge.VesperaClient", true, isolated);
            Object defaultClient = client.getMethod("create").invoke(null);
            Object admin = client.getMethod("forApp", String.class).invoke(defaultClient, "admin");
            Object request = client.getMethod("get", String.class).invoke(admin, "/users/42");
            Class<?> requestType = request.getClass();
            requestType.getMethod("query", String.class, Object.class).invoke(request, "q", "a b");
            requestType.getMethod("header", String.class, String.class).invoke(request, "Accept", "x");
            assertEquals("q=a%20b", requestType.getMethod("queryString").invoke(request));
            assertEquals("admin", requestType.getMethod("appName").invoke(request));
            Class.forName("com.devfive.vespera.bridge.VesperaResponse", true, isolated);
            Class.forName("com.devfive.vespera.bridge.VesperaClientException", true, isolated);
        }
    }
}
