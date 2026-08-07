//! HTTP method constants and utilities.
//!
//! This module provides utilities for working with HTTP methods in route attributes.
//! It handles method validation against [`HttpMethod::ALL`], the single
//! source of truth for the supported-method set (defined in `vespera_core`).
//!
//! # Overview
//!
//! HTTP methods are used in `#[vespera::route]` attributes to specify the HTTP verb
//! for a handler. This module provides validation to ensure only standard HTTP methods
//! are used.
//!
//! # Supported Methods
//!
//! The following HTTP methods are supported (case-insensitive):
//! - GET
//! - POST
//! - PUT
//! - PATCH
//! - DELETE
//! - HEAD
//! - OPTIONS
//! - TRACE
//!
//! # Key Functions
//!
//! - [`is_http_method`] - Validate if a string is a valid HTTP method

use vespera_core::route::HttpMethod;

/// Check if a string is a valid HTTP method (case-insensitive).
///
/// Returns `true` if the input string (in any case) matches one of the
/// supported HTTP methods in [`HttpMethod::ALL`], the single source of truth
/// shared with `vespera_core`'s `Display` / `TryFrom<&str>` impls.
///
/// # Example
///
/// ```ignore
/// assert!(is_http_method("GET"));
/// assert!(is_http_method("get"));
/// assert!(is_http_method("Post"));
/// assert!(!is_http_method("invalid"));
/// ```
pub fn is_http_method(s: &str) -> bool {
    // Case-insensitive match without allocating a case-folded copy of either
    // side (HTTP method names are ASCII). Deliberately NOT delegated to
    // `HttpMethod::try_from(s).is_ok()`: that allocates two `String`s per
    // non-method identifier on the `args.rs` hot path.
    HttpMethod::ALL
        .iter()
        .any(|method| s.eq_ignore_ascii_case(method.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_http_method_valid() {
        for method in HttpMethod::ALL {
            let upper = method.as_str();
            assert!(is_http_method(upper));
            assert!(is_http_method(&upper.to_lowercase()));
        }
    }

    #[test]
    fn test_is_http_method_invalid() {
        assert!(!is_http_method("invalid"));
        assert!(!is_http_method("connect"));
        assert!(!is_http_method(""));
    }

    #[test]
    fn test_http_methods_includes_trace() {
        assert!(HttpMethod::ALL.contains(&HttpMethod::Trace));
    }

    #[test]
    fn test_all_methods_parseable() {
        // Verify all methods can be parsed and recognized
        for method in HttpMethod::ALL {
            let name = method.as_str();
            assert!(is_http_method(name), "Method {name} should be recognized");
        }
    }
}
