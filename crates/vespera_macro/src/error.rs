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
