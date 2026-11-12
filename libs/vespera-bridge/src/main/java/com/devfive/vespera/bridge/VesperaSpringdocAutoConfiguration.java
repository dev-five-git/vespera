package com.devfive.vespera.bridge;

import java.util.List;
import org.springdoc.core.customizers.OpenApiCustomizer;
import org.springdoc.core.models.GroupedOpenApi;
import org.springdoc.core.properties.SpringDocConfigProperties;
import org.springdoc.core.properties.SwaggerUiConfigProperties;
import org.springdoc.core.utils.SpringDocUtils;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.beans.factory.SmartInitializingSingleton;
import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.boot.autoconfigure.condition.ConditionalOnWebApplication;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

/**
 * springdoc-openapi integration: the Rust routes show up in the application's
 * Swagger UI.  Active only when springdoc ({@code springdoc-openapi-starter-
 * webmvc-api} / {@code -ui}, an optional dependency of the bridge) is on the
 * classpath, its api-docs are enabled and
 * {@code vespera.bridge.springdoc.enabled} is not {@code false}.
 *
 * <p>The Rust app must serve its document ({@code vespera!(spec_url =
 * "/openapi.json", ...)}); it is fetched in-process by the
 * {@link VesperaOpenApiSource} bean (replaceable).  Modes
 * ({@code vespera.bridge.springdoc.mode}):
 * <ul>
 *   <li>{@code group} (default) — {@link VesperaOpenApiDocsController} serves
 *       each documented app's document verbatim at
 *       {@code /v3/api-docs/vespera/<app>} and {@link VesperaSwaggerUiUrls}
 *       adds it to Swagger UI's definition dropdown;</li>
 *   <li>{@code merge} — {@link VesperaOpenApiMerger} merges the documents into
 *       springdoc's default document.</li>
 * </ul>
 *
 * <p>Either way the proxy's catch-all {@code /**} mapping is hidden from
 * springdoc's document.
 */
@AutoConfiguration(after = VesperaBridgeAutoConfiguration.class)
@ConditionalOnClass(name = {
        "org.springdoc.core.properties.SpringDocConfigProperties",
        "org.springframework.web.servlet.DispatcherServlet"})
@ConditionalOnWebApplication(type = ConditionalOnWebApplication.Type.SERVLET)
@ConditionalOnProperty(prefix = "springdoc.api-docs", name = "enabled", matchIfMissing = true)
@EnableConfigurationProperties(VesperaBridgeProperties.class)
public class VesperaSpringdocAutoConfiguration {

    public VesperaSpringdocAutoConfiguration() {
        // The proxy's `/**` catch-all is not an API operation.
        SpringDocUtils.getConfig().addHiddenRestControllers(
                VesperaProxyController.class, VesperaOpenApiDocsController.class);
    }

    @Configuration(proxyBeanMethods = false)
    @ConditionalOnProperty(prefix = "vespera.bridge.springdoc", name = "enabled", matchIfMissing = true)
    static class Integration {

        @Bean
        @ConditionalOnMissingBean
        VesperaOpenApiSource vesperaOpenApiSource(VesperaBridgeProperties props) {
            VesperaSpringdocProperties springdoc = props.getSpringdoc();
            return VesperaOpenApiSource.inProcess(springdoc::specPathFor);
        }

        @Bean
        @ConditionalOnProperty(prefix = "vespera.bridge.springdoc", name = "mode", havingValue = "group",
                matchIfMissing = true)
        @ConditionalOnMissingBean
        VesperaOpenApiDocsController vesperaOpenApiDocsController(
                VesperaOpenApiSource source, VesperaBridgeProperties props) {
            return new VesperaOpenApiDocsController(source, documentedApps(props));
        }

        /** Lists the {@code group}-mode documents in Swagger UI (when springdoc's UI is present). */
        @Bean
        @ConditionalOnProperty(prefix = "vespera.bridge.springdoc", name = "mode", havingValue = "group",
                matchIfMissing = true)
        @ConditionalOnClass(name = "org.springdoc.core.properties.SwaggerUiConfigProperties")
        SmartInitializingSingleton vesperaSwaggerUiUrls(
                VesperaBridgeProperties props,
                ObjectProvider<SwaggerUiConfigProperties> swaggerUi,
                ObjectProvider<SpringDocConfigProperties> springDoc,
                ObjectProvider<GroupedOpenApi> groups) {
            return () -> {
                SwaggerUiConfigProperties ui = swaggerUi.getIfAvailable();
                SpringDocConfigProperties config = springDoc.getIfAvailable();
                if (ui == null || config == null) {
                    return;
                }
                boolean hasGroups = groups.stream().findAny().isPresent()
                        || (config.getGroupConfigs() != null && !config.getGroupConfigs().isEmpty());
                VesperaSwaggerUiUrls.register(ui, config, hasGroups, props.getSpringdoc(), documentedApps(props));
            };
        }

        @Bean
        @ConditionalOnProperty(prefix = "vespera.bridge.springdoc", name = "mode", havingValue = "merge")
        @ConditionalOnMissingBean(name = "vesperaOpenApiCustomizer")
        OpenApiCustomizer vesperaOpenApiCustomizer(VesperaOpenApiSource source, VesperaBridgeProperties props) {
            return new VesperaOpenApiMerger(source, documentedApps(props), props.proxyPathPatterns(),
                    props.getAppHeader());
        }

        private static List<String> documentedApps(VesperaBridgeProperties props) {
            return props.getSpringdoc().documentedApps(props.getExposedApps());
        }
    }
}
