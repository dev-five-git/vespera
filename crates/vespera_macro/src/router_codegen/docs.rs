use quote::quote;
use vespera_core::route::HttpMethod;

use crate::method::http_method_to_token_stream;

/// Which documentation page a docs route serves.
#[derive(Clone, Copy)]
pub(super) enum DocsPage {
    SwaggerUi,
    Redoc,
}

impl DocsPage {
    /// Runtime renderer (`vespera::docs::*`) that builds the page HTML from the
    /// spec JSON — asset pinning, SRI and `<script>` escaping live there.
    fn renderer(self) -> proc_macro2::TokenStream {
        match self {
            Self::SwaggerUi => quote!(vespera::docs::swagger_ui_html),
            Self::Redoc => quote!(vespera::docs::redoc_html),
        }
    }
}

/// Generate a documentation route handler (Swagger UI or ReDoc).
///
/// The page is rendered once per process (`OnceLock`) from `spec_expr`,
/// which is either the compile-time spec constant or the runtime-merged spec.
pub(super) fn generate_docs_route_tokens(
    url: &str,
    page: DocsPage,
    spec_expr: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let method_path = http_method_to_token_stream(HttpMethod::Get);
    let renderer = page.renderer();

    quote!(
        .route(#url, #method_path(|| async {
            static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let html = HTML.get_or_init(|| #renderer(#spec_expr));
            vespera::axum::response::Html(html.as_str())
        }))
    )
}

/// Generate the `spec_url` route serving the OpenAPI document as
/// `application/json`.
pub(super) fn generate_spec_route_tokens(
    url: &str,
    spec_expr: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let method_path = http_method_to_token_stream(HttpMethod::Get);

    quote!(
        .route(#url, #method_path(|| async {
            (
                [(vespera::axum::http::header::CONTENT_TYPE, "application/json")],
                #spec_expr,
            )
        }))
    )
}
