#![cfg(test)]
//! Shared test utilities for vespera_macro tests.
//!
//! This module provides helper macros and functions for writing unit tests in the vespera_macro crate.
//!
//! # Test Macros
//!
//! - [`test_fn!`] - Parse a function from Rust source code string
//! - [`test_struct!`] - Parse a struct from Rust source code string
//! - [`test_enum!`] - Parse an enum from Rust source code string
//!
//! # Test Functions
//!
//! - [`assert_schema_type`] - Assert JSON schema type field matches expected value
//! - [`create_test_temp_dir`] - Create a temporary directory for test file operations
//!
//! # Example
//!
//! ```ignore
//! #[test]
//! fn test_parsing() {
//!     let func = test_fn!("pub async fn handler() -> String { \"ok\".into() }");
//!     assert_eq!(func.sig.ident, "handler");
//! }
//! ```

/// Parse a function from source code for testing
#[macro_export]
macro_rules! test_fn {
    ($code:expr) => {{
        let file: syn::File = syn::parse_str($code).expect("parse failed");
        file.items
            .into_iter()
            .find_map(|item| {
                if let syn::Item::Fn(f) = item {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("no function found")
    }};
}

/// Parse a struct from source code for testing
#[macro_export]
macro_rules! test_struct {
    ($code:expr) => {{
        let file: syn::File = syn::parse_str($code).expect("parse failed");
        file.items
            .into_iter()
            .find_map(|item| {
                if let syn::Item::Struct(s) = item {
                    Some(s)
                } else {
                    None
                }
            })
            .expect("no struct found")
    }};
}

/// Parse an enum from source code for testing
#[macro_export]
macro_rules! test_enum {
    ($code:expr) => {{
        let file: syn::File = syn::parse_str($code).expect("parse failed");
        file.items
            .into_iter()
            .find_map(|item| {
                if let syn::Item::Enum(e) = item {
                    Some(e)
                } else {
                    None
                }
            })
            .expect("no enum found")
    }};
}

/// Assert JSON schema type
pub fn assert_schema_type(schema: &serde_json::Value, expected_type: &str) {
    assert_eq!(
        schema.get("type").and_then(|v| v.as_str()),
        Some(expected_type),
        "Schema type mismatch"
    );
}

/// Create temp directory for tests
#[allow(dead_code)]
pub fn create_test_temp_dir() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("Failed to create temp dir")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_fn_macro() {
        let f = test_fn!("fn foo() {}");
        assert_eq!(f.sig.ident, "foo");
    }

    #[test]
    fn test_test_struct_macro() {
        let s = test_struct!("struct Foo { bar: i32 }");
        assert_eq!(s.ident, "Foo");
    }

    #[test]
    fn test_test_enum_macro() {
        let e = test_enum!("enum Color { Red, Green, Blue }");
        assert_eq!(e.ident, "Color");
    }

    #[test]
    fn test_assert_schema_type() {
        let schema = serde_json::json!({"type": "string"});
        assert_schema_type(&schema, "string");
    }
}
