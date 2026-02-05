//! Unified error handling for vespera_macro.
//!
//! This module centralizes error handling for all proc-macro operations,
//! ensuring consistent span-based error reporting at compile time.
//!
//! # Overview
//!
//! All proc-macro operations should return [`MacroResult<T>`] instead of panicking,
//! allowing the Rust compiler to display user-friendly error messages with proper source locations.
//!
//! # Key Functions
//!
//! - [`err_call_site`] - Create an error at the macro call site
//! - [`err_spanned`] - Create an error at a specific AST node location
//! - [`IntoSynError`] - Convert other error types to syn::Error
//!
//! # Example
//!
//! ```ignore
//! fn process_something(input: TokenStream) -> MacroResult<TokenStream> {
//!     let data = syn::parse2(input)?;
//!     // ... validation ...
//!     if invalid {
//!         return Err(err_call_site("invalid input format"));
//!     }
//!     Ok(quote! { /* ... */ })
//! }
//! ```

use proc_macro2::Span;
use syn::Error;

/// Result type for all macro operations.
pub type MacroResult<T> = Result<T, Error>;

/// Create an error at the call site.
#[inline]
pub fn err_call_site<M: std::fmt::Display>(message: M) -> Error {
    Error::new(Span::call_site(), message)
}

// The following helpers are provided for future use when we need
// span-based errors or error conversion from other types.

/// Create an error at the given span.
#[allow(dead_code)]
#[inline]
pub fn err_spanned<T: quote::ToTokens, M: std::fmt::Display>(tokens: T, message: M) -> Error {
    Error::new_spanned(tokens, message)
}

/// Trait for converting other error types to syn::Error.
#[allow(dead_code)]
pub trait IntoSynError: Sized {
    fn into_syn_error(self, span: Span) -> Error;
    fn into_syn_error_call_site(self) -> Error {
        self.into_syn_error(Span::call_site())
    }
}

impl IntoSynError for std::io::Error {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self.to_string())
    }
}

impl IntoSynError for String {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self)
    }
}

impl IntoSynError for &str {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self)
    }
}

impl IntoSynError for serde_json::Error {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self.to_string())
    }
}

/// Extension trait for Result to convert errors with spans.
#[allow(dead_code)]
pub trait ResultExt<T, E> {
    fn map_syn_err(self, span: Span) -> MacroResult<T>;
    fn map_syn_err_call_site(self) -> MacroResult<T>;
}

impl<T, E: IntoSynError> ResultExt<T, E> for Result<T, E> {
    fn map_syn_err(self, span: Span) -> MacroResult<T> {
        self.map_err(|e| e.into_syn_error(span))
    }
    fn map_syn_err_call_site(self) -> MacroResult<T> {
        self.map_err(|e| e.into_syn_error_call_site())
    }
}

/// Extension trait for Option to convert to syn::Error.
#[allow(dead_code)]
pub trait OptionExt<T> {
    fn ok_or_syn_err<M: std::fmt::Display>(self, span: Span, message: M) -> MacroResult<T>;
    fn ok_or_syn_err_call_site<M: std::fmt::Display>(self, message: M) -> MacroResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_syn_err<M: std::fmt::Display>(self, span: Span, message: M) -> MacroResult<T> {
        self.ok_or_else(|| Error::new(span, message))
    }
    fn ok_or_syn_err_call_site<M: std::fmt::Display>(self, message: M) -> MacroResult<T> {
        self.ok_or_else(|| err_call_site(message))
    }
}
