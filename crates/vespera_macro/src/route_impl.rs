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

use std::sync::{LazyLock, Mutex};

use crate::args;
/// Metadata stored by `#[route]` for later consumption by `vespera!()`.
///
/// Each invocation of `#[route]` pushes one entry into [`ROUTE_STORAGE`].
/// The `vespera!()` macro reads this storage to supplement file-based route discovery.
#[derive(Debug, Clone)]
pub struct StoredRouteInfo {
    /// Function name (e.g., `"get_user"`)
    pub fn_name: String,
    /// HTTP method — stored for Phase 3 (skip file re-parsing)
    #[allow(dead_code)]
    pub method: Option<String>,
    /// Custom path from `path = "/{id}"` — stored for Phase 3
    #[allow(dead_code)]
    pub custom_path: Option<String>,
    /// Additional error status codes from `error_status = [400, 404]`
    pub error_status: Option<Vec<u16>>,
    /// Tags for `OpenAPI` grouping from `tags = ["users"]`
    pub tags: Option<Vec<String>>,
    /// Description from `description = "Get user by ID"`
    pub description: Option<String>,
    /// Source file path from `Span::call_site().local_file()` (requires Rust 1.88+)
    /// `None` on older Rust — collector falls back to full file parsing.
    pub file_path: Option<String>,
    /// Full function item as string for later AST re-parsing (Phase 3)
    #[allow(dead_code)]
    pub fn_item_str: String,
}

/// Global storage for route metadata collected by `#[route]` attribute macros.
/// Read by `vespera!()` to supplement file-based route discovery.
pub static ROUTE_STORAGE: LazyLock<Mutex<Vec<StoredRouteInfo>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Extract `u16` error status codes from a `syn::ExprArray`.
fn extract_error_status_codes(arr: &syn::ExprArray) -> Option<Vec<u16>> {
    let codes: Vec<u16> = arr
        .elems
        .iter()
        .filter_map(|elem| {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(lit_int),
                ..
            }) = elem
            {
                lit_int.base10_parse::<u16>().ok()
            } else {
                None
            }
        })
        .collect();
    if codes.is_empty() { None } else { Some(codes) }
}

/// Extract `String` tags from a `syn::ExprArray`.
fn extract_tag_strings(arr: &syn::ExprArray) -> Option<Vec<String>> {
    let tags: Vec<String> = arr
        .elems
        .iter()
        .filter_map(|elem| {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = elem
            {
                Some(lit_str.value())
            } else {
                None
            }
        })
        .collect();
    if tags.is_empty() { None } else { Some(tags) }
}

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
    let route_args = syn::parse2::<args::RouteArgs>(attr)?;
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| syn::Error::new(e.span(), "#[route] attribute: can only be applied to functions, not other items. Move or remove the attribute."))?;
    validate_route_fn(&item_fn)?;

    // Store route metadata for later consumption by vespera!() macro
    let stored = StoredRouteInfo {
        fn_name: item_fn.sig.ident.to_string(),
        method: route_args.method.as_ref().map(syn::Ident::to_string),
        custom_path: route_args.path.as_ref().map(syn::LitStr::value),
        error_status: route_args
            .error_status
            .as_ref()
            .and_then(extract_error_status_codes),
        tags: route_args.tags.as_ref().and_then(extract_tag_strings),
        description: route_args
            .description
            .as_ref()
            .map(syn::LitStr::value)
            .or_else(|| crate::route::extract_doc_comment(&item_fn.attrs)),
        fn_item_str: item.to_string(),
        file_path: proc_macro2::Span::call_site()
            .local_file()
            .map(|p| p.display().to_string()),
    };
    ROUTE_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(stored);

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
            assert!(result.is_ok(), "Method {method} should be valid");
        }
    }

    // ========== Tests for ROUTE_STORAGE population ==========

    #[test]
    fn test_route_storage_populated_by_process_route_attribute() {
        let attr = quote!(
            get,
            path = "/{id}",
            tags = ["users"],
            description = "Get user by ID",
            error_status = [404]
        );
        let item = quote!(
            pub async fn get_user_test_storage() -> String {
                "test".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());

        // Find our entry by unique fn_name (ROUTE_STORAGE is global, shared across parallel tests)
        let storage = ROUTE_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Find our entry and verify fields
        let stored = storage
            .iter()
            .find(|s| s.fn_name == "get_user_test_storage");
        assert!(
            stored.is_some(),
            "StoredRouteInfo should be in ROUTE_STORAGE"
        );
        let stored = stored.unwrap();
        assert_eq!(stored.method, Some("get".to_string()));
        assert_eq!(stored.custom_path, Some("/{id}".to_string()));
        assert_eq!(stored.tags, Some(vec!["users".to_string()]));
        assert_eq!(stored.description, Some("Get user by ID".to_string()));
        assert_eq!(stored.error_status, Some(vec![404]));
        assert!(stored.fn_item_str.contains("get_user_test_storage"));
    }

    #[test]
    fn test_route_storage_no_optional_fields() {
        let attr = quote!();
        let item = quote!(
            pub async fn minimal_handler_test() -> String {
                "test".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());

        let storage = ROUTE_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let stored = storage.iter().find(|s| s.fn_name == "minimal_handler_test");
        assert!(stored.is_some());
        let stored = stored.unwrap();
        assert_eq!(stored.method, None);
        assert_eq!(stored.custom_path, None);
        assert_eq!(stored.tags, None);
        assert_eq!(stored.description, None);
        assert_eq!(stored.error_status, None);
    }

    #[test]
    fn test_extract_error_status_codes_empty() {
        let arr: syn::ExprArray = syn::parse_quote!([]);
        assert_eq!(extract_error_status_codes(&arr), None);
    }

    #[test]
    fn test_extract_error_status_codes_values() {
        let arr: syn::ExprArray = syn::parse_quote!([400, 404, 500]);
        assert_eq!(extract_error_status_codes(&arr), Some(vec![400, 404, 500]));
    }

    #[test]
    fn test_extract_tag_strings_empty() {
        let arr: syn::ExprArray = syn::parse_quote!([]);
        assert_eq!(extract_tag_strings(&arr), None);
    }

    #[test]
    fn test_extract_tag_strings_values() {
        let arr: syn::ExprArray = syn::parse_quote!(["users", "admin", "api"]);
        assert_eq!(
            extract_tag_strings(&arr),
            Some(vec![
                "users".to_string(),
                "admin".to_string(),
                "api".to_string()
            ])
        );
    }
}
