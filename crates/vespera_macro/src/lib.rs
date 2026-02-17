//! Vespera macro implementation crate.
//!
//! This crate contains all the proc-macros for Vespera:
//! - `#[vespera::route(...)]` - Mark a function as a route handler
//! - `#[derive(Schema)]` - Register a type for `OpenAPI` schema generation
//! - `schema!(...)` - Get `OpenAPI` schema at compile time
//! - `vespera!(...)` - Generate Axum router with `OpenAPI`
//! - `export_app!(...)` - Export router for merging
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ Compile-time (vespera! macro)                                    │
//! ├─────────────────────────────────────────────────────────────────┤
//! │ 1. Scan src/routes/ for .rs files              [collector]       │
//! │ 2. Parse #[route] attributes                   [args, route]     │
//! │ 3. Extract handler signatures                  [parser]          │
//! │ 4. Convert Rust types → JSON Schema            [parser/schema]   │
//! │ 5. Build OpenAPI document                      [openapi_gen]     │
//! │ 6. Write openapi.json to disk                  [vespera_impl]    │
//! │ 7. Generate Axum Router TokenStream            [router_codegen]  │
//! │ 8. Inject Swagger/ReDoc HTML routes           [router_codegen]  │
//! └─────────────────────────────────────────────────────────────────┘
//!
//! # Module Organization
//!
//! - `args` - Parse `#[route(...)]` attribute arguments
//! - `collector` - Filesystem scanning and route discovery
//! - `error` - Unified error handling
//! - `http` - HTTP method constants and validation
//! - `metadata` - Type definitions for collected metadata
//! - `method` - HTTP method token stream generation
//! - `openapi_generator` - OpenAPI spec assembly
//! - `parser` - Type extraction and schema generation
//! - `route` - Route information structures
//! - `route_impl` - Route attribute macro implementation
//! - `router_codegen` - Router and macro input parsing
//! - `schema_impl` - Schema derive macro implementation
//! - `schema_macro` - `schema_type!` macro implementation
//! - `vespera_impl` - Main macro orchestration

mod args;
mod collector;
mod error;
mod file_utils;
mod http;
mod metadata;
mod method;
mod openapi_generator;
mod parse_utils;
mod parser;
mod route;
mod route_impl;
mod router_codegen;
mod schema_impl;
mod schema_macro;
mod vespera_impl;

use proc_macro::TokenStream;
pub(crate) use schema_impl::SCHEMA_STORAGE;

use crate::{
    router_codegen::{AutoRouterInput, ExportAppInput, process_vespera_input},
    vespera_impl::{process_export_app, process_vespera_macro},
};

/// route attribute macro
#[cfg(not(tarpaulin_include))]
#[proc_macro_attribute]
pub fn route(attr: TokenStream, item: TokenStream) -> TokenStream {
    match route_impl::process_route_attribute(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive macro for Schema
///
/// Supports `#[schema(name = "CustomName")]` attribute to set custom `OpenAPI` schema name.
#[cfg(not(tarpaulin_include))]
#[proc_macro_derive(Schema, attributes(schema, serde))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let (metadata, expanded) = schema_impl::process_derive_schema(&input);
    let name = metadata.name.clone();
    SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(name, metadata);
    TokenStream::from(expanded)
}

/// Generate an `OpenAPI` Schema from a type with optional field filtering.
///
/// This macro creates a `vespera::schema::Schema` struct at compile time
/// from a type that has `#[derive(Schema)]`.
///
/// # Syntax
///
/// ```ignore
/// // Full schema (all fields)
/// let user_schema = schema!(User);
///
/// // Schema with fields omitted
/// let response_schema = schema!(User, omit = ["password", "internal_id"]);
///
/// // Schema with only specified fields (pick)
/// let summary_schema = schema!(User, pick = ["id", "name"]);
/// ```
///
/// # Parameters
///
/// - `Type`: The type to generate schema for (must have `#[derive(Schema)]`)
/// - `omit = [...]`: Optional list of field names to exclude from the schema
/// - `pick = [...]`: Optional list of field names to include (excludes all others)
///
/// Note: `omit` and `pick` cannot be used together.
///
/// # Example
///
/// ```ignore
/// use vespera::{Schema, schema};
///
/// #[derive(Schema)]
/// struct User {
///     pub id: i32,
///     pub name: String,
///     pub email: String,
///     pub password: String,  // sensitive!
/// }
///
/// // For API responses, omit password
/// let response_schema = schema!(User, omit = ["password"]);
///
/// // For list endpoints, only return summary fields
/// let list_schema = schema!(User, pick = ["id", "name"]);
/// ```
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as schema_macro::SchemaInput);

    // Get stored schemas
    let storage = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match schema_macro::generate_schema_code(&input, &storage) {
        Ok(tokens) => TokenStream::from(tokens),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Generate a new struct type derived from an existing type with field filtering.
///
/// This macro creates a new struct at compile time by picking or omitting fields
/// from an existing type that has `#[derive(Schema)]`.
///
/// # Syntax
///
/// ```ignore
/// // Pick specific fields
/// schema_type!(CreateUserRequest from User, pick = ["name", "email"]);
///
/// // Omit specific fields
/// schema_type!(UserResponse from User, omit = ["password", "internal_id"]);
///
/// // Without Clone derive
/// schema_type!(UserUpdate from User, pick = ["name"], clone = false);
/// ```
///
/// # Parameters
///
/// - `NewTypeName`: The name of the new struct to generate
/// - `from SourceType`: The source type to derive from (must have `#[derive(Schema)]`)
/// - `pick = [...]`: List of field names to include (excludes all others)
/// - `omit = [...]`: List of field names to exclude
/// - `clone = bool`: Whether to derive Clone (default: true)
/// - `partial`: Make all fields `Option<T>` (fields already `Option<T>` are unchanged)
/// - `partial = [...]`: Make only listed fields `Option<T>`
///
/// Note: `omit` and `pick` cannot be used together.
///
/// # Example
///
/// ```ignore
/// use vespera::{Schema, schema_type};
///
/// #[derive(Schema)]
/// pub struct User {
///     pub id: i32,
///     pub name: String,
///     pub email: String,
///     pub password: String,
/// }
///
/// // Generate CreateUserRequest with only name and email
/// schema_type!(CreateUserRequest from User, pick = ["name", "email"]);
///
/// // Generate UserPublic without password
/// schema_type!(UserPublic from User, omit = ["password"]);
///
/// // Now use in handlers:
/// pub async fn create_user(Json(req): Json<CreateUserRequest>) -> Json<UserPublic> {
///     // ...
/// }
/// ```
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn schema_type(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as schema_macro::SchemaTypeInput);

    // Get stored schemas and generate code
    let (tokens, generated_metadata) = {
        let storage = SCHEMA_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match schema_macro::generate_schema_type_code(&input, &storage) {
            Ok(result) => result,
            Err(e) => return e.to_compile_error().into(),
        }
    };

    // If custom name is provided, register the schema directly
    // This ensures it appears in OpenAPI even when `ignore` is set
    if let Some(metadata) = generated_metadata {
        let name = metadata.name.clone();
        SCHEMA_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name, metadata);
    }
    TokenStream::from(tokens)
}

#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as AutoRouterInput);
    let processed = process_vespera_input(input);
    let schema_storage = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match process_vespera_macro(&processed, &schema_storage) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Export a vespera app as a reusable component.
///
/// Generates a struct with:
/// - `OPENAPI_SPEC: &'static str` - The `OpenAPI` JSON spec
/// - `router() -> Router` - Function returning the Axum router
///
/// # Example
/// ```ignore
/// // Simple - uses "routes" folder by default
/// vespera::export_app!(MyApp);
///
/// // Custom directory
/// vespera::export_app!(MyApp, dir = "api");
///
/// // Generates:
/// // pub struct MyApp;
/// // impl MyApp {
/// //     pub const OPENAPI_SPEC: &'static str = "...";
/// //     pub fn router() -> axum::Router { ... }
/// // }
/// ```
///
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn export_app(input: TokenStream) -> TokenStream {
    let ExportAppInput { name, dir } = syn::parse_macro_input!(input as ExportAppInput);
    let folder_name = dir
        .map(|d| d.value())
        .or_else(|| std::env::var("VESPERA_DIR").ok())
        .unwrap_or_else(|| "routes".to_string());
    let schema_storage = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "export_app! macro: CARGO_MANIFEST_DIR is not set. This macro must be used within a cargo build.",
        )
        .to_compile_error()
        .into();
    };

    match process_export_app(&name, &folder_name, &schema_storage, &manifest_dir) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
