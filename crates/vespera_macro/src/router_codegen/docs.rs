use quote::quote;
use vespera_core::route::HttpMethod;

use crate::method::http_method_to_token_stream;

/// Swagger UI HTML template. Contains `{}` format placeholder for the OpenAPI spec JSON.
pub(super) const SWAGGER_UI_HTML: &str = r##"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Swagger UI</title><link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist/swagger-ui.css" /></head><body style="margin: 0; padding: 0;"><div id="swagger-ui"></div><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js"></script><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-standalone-preset.js"></script><script>const openapiSpec = {};window.onload = () => {{ SwaggerUIBundle({{ spec: openapiSpec, dom_id: "#swagger-ui", presets: [SwaggerUIBundle.presets.apis, SwaggerUIStandalonePreset], layout: "StandaloneLayout" }}); }};</script></body></html>"##;

/// ReDoc HTML template. Contains `{}` format placeholder for the OpenAPI spec JSON.
pub(super) const REDOC_HTML: &str = r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body {{ margin: 0; padding: 0; }}</style><link rel="stylesheet" href="https://unpkg.com/redoc/bundles/redoc.standalone.css" /></head><body><div id="redoc-container"></div><script src="https://unpkg.com/redoc/bundles/redoc.standalone.js"></script><script>const openapiSpec = {};Redoc.init(openapiSpec, {{}}, document.getElementById("redoc-container"));</script></body></html>"#;

/// Generate a documentation route handler (Swagger UI or ReDoc).
///
/// When `has_merge` is true, the handler merges specs from child apps at runtime.
/// When false, it serves the spec directly from the compile-time constant.
pub(super) fn generate_docs_route_tokens(
    url: &str,
    html_template: &str,
    merge_spec_code: &[proc_macro2::TokenStream],
    has_merge: bool,
) -> proc_macro2::TokenStream {
    let method_path = http_method_to_token_stream(HttpMethod::Get);

    if has_merge {
        quote!(
            .route(#url, #method_path(|| async {
                static MERGED_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let spec = MERGED_SPEC.get_or_init(|| {
                    let mut merged: vespera::OpenApi = vespera::serde_json::from_str(__VESPERA_SPEC).unwrap();
                    #(#merge_spec_code)*
                    vespera::serde_json::to_string(&merged).unwrap()
                });
                static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let html = HTML.get_or_init(|| {
                    format!(#html_template, spec)
                });
                vespera::axum::response::Html(html.as_str())
            }))
        )
    } else {
        quote!(
            .route(#url, #method_path(|| async {
                static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let html = HTML.get_or_init(|| {
                    format!(#html_template, __VESPERA_SPEC)
                });
                vespera::axum::response::Html(html.as_str())
            }))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swagger_html_template_renders_valid_quotes() {
        assert!(
            !SWAGGER_UI_HTML.contains(r#"\""#),
            "Swagger template should not contain literal backslash-quotes: {SWAGGER_UI_HTML}"
        );
        assert!(
            SWAGGER_UI_HTML.contains(r#"href="https://unpkg.com/swagger-ui-dist/swagger-ui.css""#)
        );
        assert!(
            SWAGGER_UI_HTML
                .contains(r#"src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js""#)
        );
        assert!(SWAGGER_UI_HTML.contains(r##"dom_id: "#swagger-ui""##));
    }

    #[test]
    fn test_redoc_html_template_renders_valid_quotes() {
        assert!(
            !REDOC_HTML.contains(r#"\""#),
            "ReDoc template should not contain literal backslash-quotes: {REDOC_HTML}"
        );
        assert!(
            REDOC_HTML.contains(r#"href="https://unpkg.com/redoc/bundles/redoc.standalone.css""#)
        );
        assert!(
            REDOC_HTML.contains(r#"src="https://unpkg.com/redoc/bundles/redoc.standalone.js""#)
        );
        assert!(REDOC_HTML.contains(r#"document.getElementById("redoc-container")"#));
    }
}
