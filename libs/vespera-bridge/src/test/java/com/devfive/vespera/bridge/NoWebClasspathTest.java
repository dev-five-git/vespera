package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.File;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.Callable;
import org.junit.jupiter.api.Test;

/**
 * A non-web Spring Boot application (batch job, worker) has spring-boot and
 * spring-boot-autoconfigure but no spring-web. {@link VesperaBridgeCoreAutoConfiguration}
 * is active there, so it, {@link VesperaBridgeProperties} and the
 * {@link VesperaClient} bean must not touch a spring-web type: this test
 * re-runs them ({@link NoWebIsolationDriver}) in a class loader built from
 * the test classpath minus every web / servlet / reactive-server jar.
 */
class NoWebClasspathTest {

    private static boolean webJar(String path) {
        String name = new File(path).getName().toLowerCase(Locale.ROOT);
        return name.contains("web")
                || name.contains("servlet")
                || name.contains("tomcat")
                || name.contains("netty")
                || name.contains("springdoc")
                || name.contains("swagger")
                || name.startsWith("spring-boot-http")
                || name.startsWith("spring-boot-reactor");
    }

    @Test
    void coreAutoConfigurationRunsWithoutSpringWeb() throws Exception {
        List<URL> urls = new ArrayList<>();
        boolean droppedWeb = false;
        for (String entry : System.getProperty("java.class.path").split(File.pathSeparator)) {
            if (entry.isBlank()) {
                continue;
            }
            if (webJar(entry)) {
                droppedWeb = true;
                continue;
            }
            urls.add(new File(entry).toURI().toURL());
        }
        assertTrue(droppedWeb, "the test classpath should contain spring-web to drop");

        try (URLClassLoader isolated =
                new URLClassLoader(urls.toArray(URL[]::new), ClassLoader.getPlatformClassLoader())) {
            assertThrows(ClassNotFoundException.class, () -> Class.forName(
                    "org.springframework.web.util.pattern.PathPatternParser", false, isolated));
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("org.springframework.http.HttpHeaders", false, isolated));
            Thread thread = Thread.currentThread();
            ClassLoader previous = thread.getContextClassLoader();
            thread.setContextClassLoader(isolated);
            try {
                Callable<?> driver = (Callable<?>) Class.forName(
                        NoWebIsolationDriver.class.getName(), true, isolated)
                        .getDeclaredConstructor().newInstance();
                assertEquals("ok", driver.call());
            } finally {
                thread.setContextClassLoader(previous);
            }
        }
    }
}
