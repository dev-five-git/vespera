use proc_macro2::TokenStream;
use quote::quote;
use vespera_core::route::HttpMethod;

/// Convert HttpMethod to axum routing TokenStream
pub fn http_method_to_token_stream(method: HttpMethod) -> TokenStream {
    match method {
        HttpMethod::Get => quote! { axum::routing::get },
        HttpMethod::Post => quote! { axum::routing::post },
        HttpMethod::Put => quote! { axum::routing::put },
        HttpMethod::Patch => quote! { axum::routing::patch },
        HttpMethod::Delete => quote! { axum::routing::delete },
        HttpMethod::Head => quote! { axum::routing::head },
        HttpMethod::Options => quote! { axum::routing::options },
        HttpMethod::Trace => quote! { axum::routing::trace },
    }
}

