package com.devfive.vespera.bridge;

import org.springframework.beans.factory.ListableBeanFactory;
import org.springframework.beans.factory.config.ConfigurableBeanFactory;
import org.springframework.core.env.Environment;
import org.springframework.util.ClassUtils;

import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * The paths springdoc's Swagger UI serves, implicitly excluded from the
 * proxied URL space.
 *
 * <p>The proxy's catch-all ({@code @RequestMapping("/**")} in
 * {@code RequestMappingHandlerMapping}, order 0, or the WebFlux
 * {@link VesperaReactiveHandlerMapping}) runs before springdoc-ui's resource
 * handler ({@code SimpleUrlHandlerMapping}, {@code LOWEST_PRECEDENCE - 1}),
 * so without these excludes {@code /swagger-ui/index.html} and its assets
 * would be sent to Rust (and answered {@code 404}).
 *
 * <p>Applies only when springdoc's Swagger UI is actually configured in the
 * context: the springdoc UI configuration
 * ({@value #SERVLET_UI} / {@value #REACTIVE_UI}) is on the classpath and
 * registered as a bean, and {@code springdoc.swagger-ui.enabled} is not
 * {@code false}. The excluded paths follow {@code springdoc.swagger-ui.path}
 * (default {@code /swagger-ui.html}): the path itself plus
 * {@code <its parent>/swagger-ui/**}; WebFlux also
 * {@code <springdoc.webjars.prefix>/swagger-ui/**} (default
 * {@code /webjars}).
 */
final class SpringdocUiPaths {

    static final String SERVLET_UI = "org.springdoc.webmvc.ui.SwaggerConfig";
    static final String REACTIVE_UI = "org.springdoc.webflux.ui.SwaggerConfig";

    private SpringdocUiPaths() {
    }

    /** The implicit excludes for this context ({@code reactive}: the WebFlux UI). */
    static List<String> excludes(ListableBeanFactory beans, Environment env, boolean reactive) {
        String className = reactive ? REACTIVE_UI : SERVLET_UI;
        ClassLoader loader = beans instanceof ConfigurableBeanFactory configurable
                ? configurable.getBeanClassLoader() : null;
        if (loader == null) {
            loader = SpringdocUiPaths.class.getClassLoader();
        }
        if (!ClassUtils.isPresent(className, loader)) {
            return List.of();
        }
        Class<?> ui = ClassUtils.resolveClassName(className, loader);
        if (beans.getBeanNamesForType(ui, true, false).length == 0) {
            return List.of();
        }
        return excludes(env, reactive);
    }

    /** The UI paths per the springdoc properties, or none when the UI is disabled. */
    static List<String> excludes(Environment env, boolean reactive) {
        if (!env.getProperty("springdoc.swagger-ui.enabled", Boolean.class, Boolean.TRUE)) {
            return List.of();
        }
        String path = absolute(env.getProperty("springdoc.swagger-ui.path", "/swagger-ui.html"));
        String parent = path.substring(0, path.lastIndexOf('/'));
        Set<String> out = new LinkedHashSet<>();
        if (path.length() > 1) {
            out.add(path);
        }
        out.add(parent + "/swagger-ui/**");
        if (reactive) {
            String prefix = absolute(env.getProperty("springdoc.webjars.prefix", "/webjars"));
            out.add((prefix.equals("/") ? "" : prefix) + "/swagger-ui/**");
        }
        return List.copyOf(out);
    }

    private static String absolute(String path) {
        String trimmed = path == null ? "" : path.trim();
        while (trimmed.length() > 1 && trimmed.endsWith("/")) {
            trimmed = trimmed.substring(0, trimmed.length() - 1);
        }
        return trimmed.startsWith("/") ? trimmed : "/" + trimmed;
    }
}
