package com.devfive.vespera.bridge;

import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import org.springdoc.core.properties.AbstractSwaggerUiConfigProperties.SwaggerUrl;
import org.springdoc.core.properties.SpringDocConfigProperties;
import org.springdoc.core.properties.SwaggerUiConfigProperties;

/**
 * {@code vespera.bridge.springdoc.mode=group}: adds one Swagger UI
 * definition ({@code springdoc.swagger-ui.urls}) per documented Rust app,
 * pointing at {@link VesperaOpenApiDocsController}.
 *
 * <p>Swagger UI ignores its single {@code url} once {@code urls} is set, so
 * when the application configured neither {@code urls} nor springdoc groups,
 * springdoc's own document ({@code springdoc.swagger-ui.url}, else
 * {@code springdoc.api-docs.path}) is listed too, as {@value #APPLICATION},
 * and stays the initially selected definition.  With springdoc groups the
 * groups are listed by springdoc itself, next to the vespera entries.
 */
final class VesperaSwaggerUiUrls {

    /** Swagger UI name of springdoc's own (Java) document. */
    static final String APPLICATION = "application";

    private VesperaSwaggerUiUrls() {}

    /**
     * Register the vespera entries on {@code swaggerUi}.  Entries with the
     * same name already configured by the application are kept as they are.
     *
     * @param hasGroups whether springdoc serves grouped documents
     */
    static void register(SwaggerUiConfigProperties swaggerUi, SpringDocConfigProperties springDoc,
                         boolean hasGroups, VesperaSpringdocProperties props, List<String> apps) {
        Set<SwaggerUrl> urls = new LinkedHashSet<>();
        if (swaggerUi.getUrls() != null) {
            urls.addAll(swaggerUi.getUrls());
        }
        if (urls.isEmpty() && !hasGroups) {
            String own = swaggerUi.getUrl();
            if (own == null || own.isBlank()) {
                own = springDoc.getApiDocs().getPath();
            }
            urls.add(new SwaggerUrl(APPLICATION, own, APPLICATION));
            if (swaggerUi.getUrlsPrimaryName() == null || swaggerUi.getUrlsPrimaryName().isBlank()) {
                swaggerUi.setUrlsPrimaryName(APPLICATION);
            }
        }
        String base = props.validatedDocsPath();
        for (String app : apps) {
            String name = groupName(app);
            if (urls.stream().noneMatch(url -> name.equals(url.getName()))) {
                urls.add(new SwaggerUrl(name, base + "/" + app, props.displayNameFor(app)));
            }
        }
        swaggerUi.setUrls(urls);
    }

    /** Swagger UI definition name of {@code app}: {@code vespera} / {@code vespera-<app>}. */
    static String groupName(String app) {
        return AppNameResolver.DEFAULT_APP.equals(app) ? "vespera" : "vespera-" + app;
    }
}
