use proc_macro2::TokenStream;
use quote::quote;
use vespera_core::route::HttpMethod;

/// Convert HttpMethod to axum routing TokenStream
pub fn http_method_to_token_stream(method: HttpMethod) -> TokenStream {
    match method {
        HttpMethod::Get => quote! { vespera::axum::routing::get },
        HttpMethod::Post => quote! { vespera::axum::routing::post },
        HttpMethod::Put => quote! { vespera::axum::routing::put },
        HttpMethod::Patch => quote! { vespera::axum::routing::patch },
        HttpMethod::Delete => quote! { vespera::axum::routing::delete },
        HttpMethod::Head => quote! { vespera::axum::routing::head },
        HttpMethod::Options => quote! { vespera::axum::routing::options },
        HttpMethod::Trace => quote! { vespera::axum::routing::trace },
    }
}
