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

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

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

/// Per-crate storage for cron metadata collected by `#[cron]` attribute
/// macros, read by `vespera!()` to build the cron scheduler.
///
/// Keyed by [`crate::schema_impl::current_crate_key`] so a long-lived
/// rust-analyzer proc-macro server (one process, many crates) never schedules
/// crate A's cron jobs into crate B. See
/// [`SCHEMA_STORAGE`](crate::schema_impl::SCHEMA_STORAGE) for the rationale.
pub static CRON_STORAGE: LazyLock<Mutex<HashMap<String, Arc<Vec<StoredCronInfo>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn same_cron_source(left: &StoredCronInfo, right: &StoredCronInfo) -> bool {
    left.fn_name == right.fn_name
        && crate::file_utils::paths_equal_normalized(
            left.file_path.as_deref(),
            right.file_path.as_deref(),
        )
}

/// Replace-insert a `#[cron]` metadata entry in the current crate's bucket.
pub fn register_cron(info: StoredCronInfo) {
    let mut guard = CRON_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bucket = Arc::make_mut(
        guard
            .entry(crate::schema_impl::current_crate_key())
            .or_insert_with(|| Arc::new(Vec::new())),
    );
    if let Some(existing) = bucket
        .iter_mut()
        .find(|existing| same_cron_source(existing, &info))
    {
        *existing = info;
    } else {
        bucket.push(info);
    }
}

/// Snapshot of the current crate's registered cron jobs — a cheap `Arc` clone.
#[must_use]
pub fn current_crate_crons() -> Arc<Vec<StoredCronInfo>> {
    CRON_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&crate::schema_impl::current_crate_key())
        .cloned()
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

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
    // Compile-time cron-syntax validation (gated by the `cron` feature, enabled
    // transitively by `vespera`'s `cron` feature). A malformed expression is a
    // span-attached compile error here instead of a `JobScheduler` panic at app
    // startup (see `router_codegen::generator`'s `Job::new_async(...).expect`).
    #[cfg(feature = "cron")]
    validate_cron_expression(&expression)?;
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| syn::Error::new(e.span(), "#[cron] attribute: can only be applied to functions, not other items. Move or remove the attribute."))?;
    validate_cron_fn(&item_fn)?;

    let stored = StoredCronInfo {
        fn_name: item_fn.sig.ident.to_string(),
        expression: expression.value(),
        file_path: proc_macro2::Span::call_site()
            .local_file()
            .map(|p| p.display().to_string()),
    };
    register_cron(stored);

    Ok(item)
}

/// Validate a cron expression at **compile time** using the SAME parser the
/// runtime uses, so a malformed expression is a clean span-attached compile
/// error instead of a `JobScheduler` panic at application startup.
///
/// Parity basis: `tokio-cron-scheduler`'s `Job::new_async` parses the schedule
/// with `croner`'s `CronParser::builder().seconds(Seconds::Required).build()`.
/// `vespera` enables `tokio-cron-scheduler` **without** its `english` feature,
/// so the runtime `schedule_to_cron` step is an identity passthrough and the
/// only parse is the 6-field (seconds-required) croner parse replicated here.
/// The `croner` major version is pinned (in `Cargo.toml`) to the one
/// `tokio-cron-scheduler` resolves, so compile-time acceptance exactly matches
/// runtime acceptance.
#[cfg(feature = "cron")]
fn validate_cron_expression(expression: &syn::LitStr) -> syn::Result<()> {
    use croner::parser::{CronParser, Seconds};
    let expr = expression.value();
    CronParser::builder()
        .seconds(Seconds::Required)
        .build()
        .parse(&expr)
        .map_err(|e| {
            syn::Error::new_spanned(
                expression,
                format!(
                    "#[cron] invalid cron expression `{expr}`: {e}. Expected a 6-field \
                     expression `sec min hour day month weekday`, e.g. \"0 */5 * * * *\"."
                ),
            )
        })?;
    Ok(())
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

    #[test]
    fn test_register_cron_replaces_same_file_and_function() {
        let file_path = Some("/tmp/vespera/tasks/replaced.rs".to_string());
        let fn_name = "__test_replace_cron".to_string();
        register_cron(StoredCronInfo {
            fn_name: fn_name.clone(),
            expression: "0 */5 * * * *".to_string(),
            file_path: file_path.clone(),
        });
        register_cron(StoredCronInfo {
            fn_name: fn_name.clone(),
            expression: "0 */10 * * * *".to_string(),
            file_path,
        });

        let matches: Vec<_> = current_crate_crons()
            .iter()
            .filter(|entry| entry.fn_name == fn_name)
            .cloned()
            .collect();
        assert_eq!(matches.len(), 1, "same source cron should replace");
        assert_eq!(matches[0].expression, "0 */10 * * * *");
    }

    // ===== Compile-time cron-syntax validation (gated by the `cron` feature) =====

    #[cfg(feature = "cron")]
    #[test]
    fn test_process_cron_attribute_valid_cron_syntax_passes() {
        for expr in [
            quote!("0 */5 * * * *"),
            quote!("1/10 * * * * *"),
            quote!("0 0 0 * * *"),
            quote!("0 30 9 * * Mon-Fri"),
        ] {
            let item = quote!(
                pub async fn my_job() {}
            );
            assert!(
                process_cron_attribute(expr.clone(), item).is_ok(),
                "expected valid cron `{expr}` to pass"
            );
        }
    }

    #[cfg(feature = "cron")]
    #[test]
    fn test_process_cron_attribute_invalid_cron_syntax_is_compile_error() {
        // Each is rejected at compile time (was a runtime `JobScheduler` panic):
        // 1-field, 5-field (missing seconds), out-of-range minute, garbage token.
        for bad in [
            quote!("invalid"),
            quote!("* * * * *"),
            quote!("0 99 * * * *"),
            quote!("not a cron at all"),
        ] {
            let item = quote!(
                pub async fn my_job() {}
            );
            let result = process_cron_attribute(bad.clone(), item);
            assert!(result.is_err(), "expected invalid cron `{bad}` to error");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("invalid cron expression"),
                "expected `invalid cron expression` message for `{bad}`"
            );
        }
    }
}
