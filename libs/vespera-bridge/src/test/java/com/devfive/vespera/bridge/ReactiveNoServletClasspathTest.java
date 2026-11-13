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
 * A WebFlux application has no servlet API and no Spring MVC on its
 * classpath. The reactive proxy (handler, resolvers, header policy, wire
 * codec, autoconfiguration) must load and run there: this test re-runs it
 * ({@link ReactiveIsolationDriver}) in a class loader built from the test
 * classpath minus the servlet API, Tomcat and Spring MVC jars, so a stray
 * servlet-typed reference surfaces as {@link NoClassDefFoundError}.
 */
class ReactiveNoServletClasspathTest {

    private static boolean servletOrMvc(String path) {
        String name = new File(path).getName().toLowerCase(Locale.ROOT);
        return name.startsWith("jakarta.servlet-api")
                || name.startsWith("tomcat-")
                || name.startsWith("spring-webmvc")
                || name.startsWith("spring-boot-webmvc")
                || name.startsWith("spring-boot-servlet")
                || name.startsWith("spring-boot-tomcat")
                || name.startsWith("spring-boot-starter-tomcat")
                || name.startsWith("springdoc-openapi-starter-webmvc");
    }

    @Test
    void reactiveProxyRunsWithoutTheServletApiOrSpringMvc() throws Exception {
        List<URL> urls = new ArrayList<>();
        boolean droppedServlet = false;
        for (String entry : System.getProperty("java.class.path").split(File.pathSeparator)) {
            if (entry.isBlank()) {
                continue;
            }
            if (servletOrMvc(entry)) {
                droppedServlet = true;
                continue;
            }
            urls.add(new File(entry).toURI().toURL());
        }
        assertTrue(droppedServlet, "the test classpath should contain the servlet API to drop");

        try (URLClassLoader isolated =
                new URLClassLoader(urls.toArray(URL[]::new), ClassLoader.getPlatformClassLoader())) {
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("jakarta.servlet.http.HttpServletRequest", false, isolated));
            assertThrows(ClassNotFoundException.class,
                    () -> Class.forName("org.springframework.web.servlet.DispatcherServlet", false, isolated));
            Thread thread = Thread.currentThread();
            ClassLoader previous = thread.getContextClassLoader();
            thread.setContextClassLoader(isolated);
            try {
                Callable<?> driver = (Callable<?>) Class.forName(
                        ReactiveIsolationDriver.class.getName(), true, isolated)
                        .getDeclaredConstructor().newInstance();
                assertEquals("ok", driver.call());
            } finally {
                thread.setContextClassLoader(previous);
            }
        }
    }
}
