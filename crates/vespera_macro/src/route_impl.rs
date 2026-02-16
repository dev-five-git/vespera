//! Route attribute macro implementation.
//!
//! This module implements the `#[vespera::route]` attribute macro that validates
//! and processes handler functions for route registration.
//!
//! # Overview
//!
//! The `#[route]` attribute is applied to handler functions to:
//! - Validate that the function is `pub async fn`
//! - Parse route configuration (HTTP method, path, tags, etc.)
//! - Mark the function for route discovery by the `vespera!` macro
//!
//! # Route Requirements
//!
//! All handler functions must:
//! - Be public (`pub`)
//! - Be async (`async fn`)
//! - Accept standard Axum extractors (Path, Query, Json, etc.)
//! - Return a response type (Json, String, `StatusCode`, etc.)
//!
//! # Key Functions
//!
//! - [`validate_route_fn`] - Validate route function signature
//! - [`process_route_attribute`] - Parse and process the route attribute
//!
//! # Example
//!
//! ```ignore
//! #[vespera::route(get, path = "/{id}", tags = ["users"])]
//! pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
//!     Json(User { id, name: "Alice".into() })
//! }
//! ```

use crate::args;

/// Validate route function - must be pub and async
pub fn validate_route_fn(item_fn: &syn::ItemFn) -> Result<(), syn::Error> {
    if !matches!(item_fn.vis, syn::Visibility::Public(_)) {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[route] attribute: function must be public. Add `pub` before `fn`.",
        ));
    }
    if item_fn.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[route] attribute: function must be async. Add `async` before `fn`.",
        ));
    }
    Ok(())
}

/// Process route attribute - extracted for testability
pub fn process_route_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    syn::parse2::<args::RouteArgs>(attr)?;
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| syn::Error::new(e.span(), "#[route] attribute: can only be applied to functions, not other items. Move or remove the attribute."))?;
    validate_route_fn(&item_fn)?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    // ========== Tests for validate_route_fn ==========

    #[test]
    fn test_validate_route_fn_not_public() {
        let item: syn::ItemFn = syn::parse_quote! {
            async fn private_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be public")
        );
    }

    #[test]
    fn test_validate_route_fn_not_async() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub fn sync_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be async")
        );
    }

    #[test]
    fn test_validate_route_fn_valid() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub async fn valid_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_ok());
    }

    // ========== Tests for process_route_attribute ==========

    #[test]
    fn test_process_route_attribute_valid() {
        let attr = quote!(get);
        let item = quote!(
            pub async fn handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item.clone());
        assert!(result.is_ok());
        // Should return the original item unchanged
        assert_eq!(result.unwrap().to_string(), item.to_string());
    }

    #[test]
    fn test_process_route_attribute_invalid_attr() {
        let attr = quote!(invalid_method);
        let item = quote!(
            pub async fn handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_route_attribute_not_function() {
        let attr = quote!(get);
        let item = quote!(
            struct NotAFunction;
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("can only be applied to functions"));
    }

    #[test]
    fn test_process_route_attribute_not_public() {
        let attr = quote!(get);
        let item = quote!(
            async fn private_handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be public"));
    }

    #[test]
    fn test_process_route_attribute_not_async() {
        let attr = quote!(get);
        let item = quote!(
            pub fn sync_handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be async"));
    }

    #[test]
    fn test_process_route_attribute_with_path() {
        let attr = quote!(get, path = "/users/{id}");
        let item = quote!(
            pub async fn get_user() -> String {
                "user".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_route_attribute_with_tags() {
        let attr = quote!(post, tags = ["users", "admin"]);
        let item = quote!(
            pub async fn create_user() -> String {
                "created".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_route_attribute_all_methods() {
        let methods = ["get", "post", "put", "patch", "delete", "head", "options"];
        for method in methods {
            let attr: proc_macro2::TokenStream = method.parse().unwrap();
            let item = quote!(
                pub async fn handler() -> String {
                    "ok".to_string()
                }
            );
            let result = process_route_attribute(attr, item);
            assert!(result.is_ok(), "Method {} should be valid", method);
        }
    }
}
