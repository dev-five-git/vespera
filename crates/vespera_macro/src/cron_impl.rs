//! Cron attribute macro implementation.
//!
//! This module implements the `#[vespera::cron]` attribute macro that validates
//! and processes functions for cron job registration.
//!
//! # Overview
//!
//! The `#[cron]` attribute is applied to functions to:
//! - Validate that the function is `pub async fn`
//! - Validate that the function takes no parameters
//! - Parse the cron expression string
//! - Mark the function for cron discovery by the `vespera!` macro
//!
//! # Example
//!
//! ```ignore
//! #[vespera::cron("0 */5 * * * *")]
//! pub async fn cleanup_sessions() {
//!     println!("Running cleanup");
//! }
//! ```

use std::sync::{LazyLock, Mutex};

/// Metadata stored by `#[cron]` for later consumption by `vespera!()`.
///
/// Each invocation of `#[cron]` pushes one entry into [`CRON_STORAGE`].
/// The `vespera!()` macro reads this storage to build the cron scheduler.
#[derive(Debug, Clone)]
pub struct StoredCronInfo {
    /// Function name (e.g., `"cleanup_sessions"`)
    pub fn_name: String,
    /// Cron expression (e.g., `"0 */5 * * * *"`)
    pub expression: String,
    /// Source file path from `Span::call_site().local_file()`
    pub file_path: Option<String>,
}

/// Global storage for cron metadata collected by `#[cron]` attribute macros.
/// Read by `vespera!()` to build the cron scheduler.
pub static CRON_STORAGE: LazyLock<Mutex<Vec<StoredCronInfo>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Validate cron function - must be pub, async, and take no parameters.
pub fn validate_cron_fn(item_fn: &syn::ItemFn) -> Result<(), syn::Error> {
    if !matches!(item_fn.vis, syn::Visibility::Public(_)) {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[cron] attribute: function must be public. Add `pub` before `fn`.",
        ));
    }
    if item_fn.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[cron] attribute: function must be async. Add `async` before `fn`.",
        ));
    }
    if !item_fn.sig.inputs.is_empty() {
        return Err(syn::Error::new_spanned(
            &item_fn.sig.inputs,
            "#[cron] attribute: cron functions must take no parameters.",
        ));
    }
    Ok(())
}

/// Process cron attribute - extracted for testability
pub fn process_cron_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let expression: syn::LitStr = syn::parse2(attr).map_err(|_| syn::Error::new(proc_macro2::Span::call_site(), "#[cron] attribute: expected a cron expression string. Example: #[cron(\"0 */5 * * * *\")]"))?;
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| syn::Error::new(e.span(), "#[cron] attribute: can only be applied to functions, not other items. Move or remove the attribute."))?;
    validate_cron_fn(&item_fn)?;

    let stored = StoredCronInfo {
        fn_name: item_fn.sig.ident.to_string(),
        expression: expression.value(),
        file_path: proc_macro2::Span::call_site()
            .local_file()
            .map(|p| p.display().to_string()),
    };
    CRON_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(stored);

    Ok(item)
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    // ========== Tests for validate_cron_fn ==========

    #[test]
    fn test_validate_cron_fn_not_public() {
        let item: syn::ItemFn = syn::parse_quote! {
            async fn private_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be public")
        );
    }

    #[test]
    fn test_validate_cron_fn_not_async() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub fn sync_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be async")
        );
    }

    #[test]
    fn test_validate_cron_fn_has_params() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub async fn job_with_params(x: i32) {
                println!("{}", x);
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must take no parameters")
        );
    }

    #[test]
    fn test_validate_cron_fn_valid() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub async fn valid_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_ok());
    }

    // ========== Tests for process_cron_attribute ==========

    #[test]
    fn test_process_cron_attribute_valid() {
        let attr = quote!("0 */5 * * * *");
        let item = quote!(
            pub async fn my_job() {
                println!("running");
            }
        );
        let result = process_cron_attribute(attr, item.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().to_string(), item.to_string());
    }

    #[test]
    fn test_process_cron_attribute_invalid_expression() {
        let attr = quote!(123);
        let item = quote!(
            pub async fn my_job() {
                println!("running");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected a cron expression string"));
    }

    #[test]
    fn test_process_cron_attribute_not_function() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            struct NotAFunction;
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("can only be applied to functions"));
    }

    #[test]
    fn test_process_cron_attribute_not_public() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            async fn private_job() {
                println!("job");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be public"));
    }

    #[test]
    fn test_process_cron_attribute_not_async() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            pub fn sync_job() {
                println!("job");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be async"));
    }

    #[test]
    fn test_process_cron_attribute_with_params() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            pub async fn job(x: i32) {
                println!("{}", x);
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must take no parameters"));
    }
}
