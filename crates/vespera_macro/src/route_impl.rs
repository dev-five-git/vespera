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

use crate::{args, metadata::HeaderParam};
/// Metadata stored by `#[route]` for later consumption by `vespera!()`.
///
/// Each invocation of `#[route]` pushes one entry into [`ROUTE_STORAGE`].
/// The `vespera!()` macro reads this storage to supplement file-based
/// route discovery — when `file_path` is populated, the collector can
/// build route metadata directly from this struct without re-parsing
/// the source file with `syn::parse_file()`.
#[derive(Debug, Clone)]
pub struct StoredRouteInfo {
    /// Function name (e.g., `"get_user"`).
    pub fn_name: String,
    /// HTTP method (e.g., `"get"`, `"post"`).  Used by the collector's
    /// fast path ([`crate::collector`]) to populate `RouteMetadata.method`
    /// without re-parsing the source file.
    pub method: Option<String>,
    /// Custom path from `path = "/{id}"`.  Used by the collector to
    /// derive the full route URL when present.
    pub custom_path: Option<String>,
    /// Declared non-200 success status from `status = <u16>` (validated 2xx).
    pub success_status: Option<u16>,
    /// Additional error status codes from `error_status = [400, 404]`.
    pub error_status: Option<Vec<u16>>,
    /// Typed error responses from `responses = [(404, NotFoundError)]`.
    pub typed_responses: Option<Vec<(u16, String)>>,
    /// Tags for `OpenAPI` grouping from `tags = ["users"]`.
    pub tags: Option<Vec<String>>,
    /// Per-route security requirements from `security = ["bearerAuth"]`.
    pub security: Option<Vec<String>>,
    /// Header parameters from `headers = [{ name = "Authorization" }]`.
    pub headers: Vec<HeaderParam>,
    /// Explicit OpenAPI operationId from `operation_id = "getUser"`.
    pub operation_id: Option<String>,
    /// OpenAPI operation summary from `summary = "Get user"`.
    pub summary: Option<String>,
    /// Operation-level request example.
    pub request_example: Option<serde_json::Value>,
    /// Operation-level response example.
    pub response_example: Option<serde_json::Value>,
    /// Whether the operation is deprecated via bare `deprecated`.
    pub deprecated: bool,
    /// Description from `description = "Get user by ID"`.
    pub description: Option<String>,
    /// Source file path from `Span::call_site().local_file()` (requires Rust 1.88+).
    /// `None` on older Rust — collector falls back to full file parsing.
    pub file_path: Option<String>,
    /// Function signature as a string. Re-parsed via `syn::parse_str()` by
    /// [`crate::openapi_generator`] when the source file AST is unavailable.
    /// Stores only `syn::Signature` tokens, not the handler body.
    pub fn_sig_str: String,
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

/// Extract security scheme names from a `syn::ExprArray`.
///
/// Unlike tags, an empty array is meaningful: `security = []` disables
/// inherited/global security for that operation in OpenAPI.
fn extract_security_strings(arr: &syn::ExprArray) -> Vec<String> {
    arr.elems
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
        .collect()
}

fn parse_example_string(lit: &syn::LitStr) -> serde_json::Value {
    let value = lit.value();
    serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
}

/// Extract typed response status/schema pairs from `responses = [(404, NotFoundError)]`.
fn extract_typed_responses(arr: &syn::ExprArray) -> Option<Vec<(u16, String)>> {
    let responses: Vec<(u16, String)> = arr
        .elems
        .iter()
        .filter_map(|elem| {
            let syn::Expr::Tuple(tuple) = elem else {
                return None;
            };
            let status = tuple.elems.first().and_then(|status| {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(lit_int),
                    ..
                }) = status
                {
                    lit_int.base10_parse::<u16>().ok()
                } else {
                    None
                }
            })?;
            let schema_name = tuple.elems.get(1).and_then(|schema| {
                if let syn::Expr::Path(path) = schema {
                    path.path.segments.last().map(|seg| seg.ident.to_string())
                } else {
                    None
                }
            })?;
            Some((status, schema_name))
        })
        .collect();

    if responses.is_empty() {
        None
    } else {
        Some(responses)
    }
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
    let fn_sig = &item_fn.sig;

    // Store route metadata for later consumption by vespera!() macro
    let stored = StoredRouteInfo {
        fn_name: item_fn.sig.ident.to_string(),
        method: route_args.method.as_ref().map(syn::Ident::to_string),
        custom_path: route_args.path.as_ref().map(syn::LitStr::value),
        success_status: route_args.success_status,
        error_status: route_args
            .error_status
            .as_ref()
            .and_then(extract_error_status_codes),
        typed_responses: route_args
            .responses
            .as_ref()
            .and_then(extract_typed_responses),
        tags: route_args.tags.as_ref().and_then(extract_tag_strings),
        security: route_args.security.as_ref().map(extract_security_strings),
        headers: route_args.headers.unwrap_or_default(),
        operation_id: route_args.operation_id.as_ref().map(syn::LitStr::value),
        summary: route_args.summary.as_ref().map(syn::LitStr::value),
        request_example: route_args
            .request_example
            .as_ref()
            .map(parse_example_string),
        response_example: route_args
            .response_example
            .as_ref()
            .map(parse_example_string),
        deprecated: route_args.deprecated,
        description: route_args
            .description
            .as_ref()
            .map(syn::LitStr::value)
            .or_else(|| crate::route::extract_doc_comment(&item_fn.attrs)),
        fn_sig_str: quote::quote!(#fn_sig).to_string(),
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
        assert!(stored.headers.is_empty());
        assert!(stored.fn_sig_str.contains("get_user_test_storage"));
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
        assert!(stored.headers.is_empty());
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
