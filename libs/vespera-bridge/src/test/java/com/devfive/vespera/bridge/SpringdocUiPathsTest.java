package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.support.DefaultListableBeanFactory;
import org.springframework.beans.factory.support.RootBeanDefinition;
import org.springframework.mock.env.MockEnvironment;

/** {@link SpringdocUiPaths}: which Swagger UI paths are implicitly excluded from the proxy. */
class SpringdocUiPathsTest {

    @Test
    void defaultPathExcludesTheRedirectAndTheUiResources() {
        assertEquals(List.of("/swagger-ui.html", "/swagger-ui/**"),
                SpringdocUiPaths.excludes(new MockEnvironment(), false));
        assertEquals(List.of("/swagger-ui.html", "/swagger-ui/**", "/webjars/swagger-ui/**"),
                SpringdocUiPaths.excludes(new MockEnvironment(), true));
    }

    @Test
    void customPathKeepsItsParentPrefix() {
        MockEnvironment env = new MockEnvironment()
                .withProperty("springdoc.swagger-ui.path", "docs/api/ui.html")
                .withProperty("springdoc.webjars.prefix", "/assets/");
        assertEquals(List.of("/docs/api/ui.html", "/docs/api/swagger-ui/**"),
                SpringdocUiPaths.excludes(env, false));
        assertEquals(List.of("/docs/api/ui.html", "/docs/api/swagger-ui/**", "/assets/swagger-ui/**"),
                SpringdocUiPaths.excludes(env, true));
        assertEquals(List.of("/swagger-ui/**"), SpringdocUiPaths.excludes(
                new MockEnvironment().withProperty("springdoc.swagger-ui.path", "/"), false));
    }

    @Test
    void disabledUiExcludesNothing() {
        assertEquals(List.of(), SpringdocUiPaths.excludes(
                new MockEnvironment().withProperty("springdoc.swagger-ui.enabled", "false"), false));
    }

    @Test
    void onlyWhenSpringdocsUiConfigurationIsABean() throws Exception {
        DefaultListableBeanFactory beans = new DefaultListableBeanFactory();
        assertTrue(SpringdocUiPaths.excludes(beans, new MockEnvironment(), false).isEmpty(),
                "springdoc-ui on the classpath but not configured");
        beans.registerBeanDefinition(SpringdocUiPaths.SERVLET_UI,
                new RootBeanDefinition(Class.forName(SpringdocUiPaths.SERVLET_UI)));
        assertEquals(List.of("/swagger-ui.html", "/swagger-ui/**"),
                SpringdocUiPaths.excludes(beans, new MockEnvironment(), false));
        assertFalse(SpringdocUiPaths.excludes(beans, new MockEnvironment(), true).iterator().hasNext(),
                "the WebFlux UI is not on this classpath");
    }

    @Test
    void implicitExcludesAddToTheConfiguredOnes() {
        VesperaBridgeProperties props = new VesperaBridgeProperties();
        props.setExcludePatterns(List.of("/internal/**"));
        ProxyPathPatterns patterns = props.proxyPathPatterns(List.of("/swagger-ui/**"));
        assertEquals(List.of("/internal/**", "/swagger-ui/**"), patterns.exclude());
        assertFalse(patterns.matchesPath("/swagger-ui/index.html"));
        assertTrue(patterns.matchesPath("/users"));
        assertTrue(new VesperaBridgeProperties().proxyPathPatterns(List.of()).isDefault());
    }
}
