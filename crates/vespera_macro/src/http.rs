//! HTTP method constants and utilities.
//!
//! This module provides utilities for working with HTTP methods in route attributes.
//! It handles method validation and constant definitions for all standard HTTP verbs.
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

/// All supported HTTP methods as lowercase strings.
pub const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

/// Check if a string is a valid HTTP method (case-insensitive).
///
/// Returns `true` if the input string (in any case) matches one of the
/// supported HTTP methods defined in [`HTTP_METHODS`].
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
    HTTP_METHODS.contains(&s.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_http_method_valid() {
        for method in HTTP_METHODS {
            assert!(is_http_method(method));
            assert!(is_http_method(&method.to_uppercase()));
            assert!(is_http_method(method.as_ref()));
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
        assert!(HTTP_METHODS.contains(&"trace"));
    }

    #[test]
    fn test_all_methods_parseable() {
        // Verify all methods can be parsed and recognized
        for method in HTTP_METHODS {
            assert!(
                is_http_method(method),
                "Method {} should be recognized",
                method
            );
        }
    }
}
