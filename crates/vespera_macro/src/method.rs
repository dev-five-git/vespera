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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(HttpMethod::Get, "get")]
    #[case(HttpMethod::Post, "post")]
    #[case(HttpMethod::Put, "put")]
    #[case(HttpMethod::Patch, "patch")]
    #[case(HttpMethod::Delete, "delete")]
    #[case(HttpMethod::Head, "head")]
    #[case(HttpMethod::Options, "options")]
    #[case(HttpMethod::Trace, "trace")]
    fn test_http_method_to_token_stream(
        #[case] method: HttpMethod,
        #[case] expected_method_name: &str,
    ) {
        let result = http_method_to_token_stream(method);
        let code = result.to_string();

        // Check that the code contains the expected method name
        // quote! generates "vespera :: axum :: routing :: get" format
        assert!(
            code.contains(expected_method_name),
            "Code should contain method name: {}, got: {}",
            expected_method_name,
            code
        );

        // Check that it contains the routing path
        assert!(
            code.contains("routing"),
            "Code should contain 'routing', got: {}",
            code
        );

        // Check that it contains the axum path
        assert!(
            code.contains("axum"),
            "Code should contain 'axum', got: {}",
            code
        );

        // Check that it contains the vespera path
        assert!(
            code.contains("vespera"),
            "Code should contain 'vespera', got: {}",
            code
        );
    }

    #[test]
    fn test_http_method_to_token_stream_all_methods() {
        // Test that all methods generate valid TokenStreams
        let methods = vec![
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
            HttpMethod::Head,
            HttpMethod::Options,
            HttpMethod::Trace,
        ];

        for method in methods {
            let result = http_method_to_token_stream(method.clone());
            let code = result.to_string();

            // Each should generate a valid TokenStream
            assert!(
                !code.is_empty(),
                "TokenStream should not be empty for {:?}",
                method
            );
            assert!(
                code.contains("routing"),
                "TokenStream should contain 'routing' for {:?}, got: {}",
                method,
                code
            );
        }
    }
}
