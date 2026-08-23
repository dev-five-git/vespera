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
//! - `macro_storage` - Shared per-crate storage for `#[route]` / `#[cron]` metadata
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
mod cron_impl;
mod error;
mod file_utils;
mod garde_emit;
mod http;
mod macro_storage;
mod metadata;
mod method;
mod openapi_generator;

mod multipart_impl;
mod parser;
mod route;
mod route_impl;
mod router_codegen;
mod schema_assertions;
mod schema_impl;
mod schema_macro;
mod vespera_impl;

use proc_macro::TokenStream;

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

/// cron attribute macro
///
/// Mark a function as a cron job with the given cron expression.
///
/// # Example
/// ```ignore
/// #[vespera::cron("0 */5 * * * *")]
/// pub async fn cleanup_sessions() {
///     println!("Running cleanup");
/// }
/// ```
#[cfg(not(tarpaulin_include))]
#[proc_macro_attribute]
pub fn cron(attr: TokenStream, item: TokenStream) -> TokenStream {
    match cron_impl::process_cron_attribute(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive macro for Schema
///
/// Supports `#[schema(name = "CustomName")]` attribute to set custom `OpenAPI` schema name.
///
/// # Duplicate schema name detection
///
/// `SCHEMA_STORAGE` is keyed by the OpenAPI schema name (struct ident by
/// default, or `#[schema(name = "...")]` if specified).  When two
/// **different** struct definitions register under the same name, only
/// the last one would survive in `openapi.json` — a silent footgun
/// that has bitten real users.  This derive therefore checks the
/// storage before inserting and emits a `compile_error!` so the
/// conflict surfaces at build time instead of at spec-generation time.
///
/// Identical re-registrations (e.g. incremental rebuilds running the
/// same derive twice) are idempotent: the definition token-stream
/// matches and the second call is a no-op.
#[cfg(not(tarpaulin_include))]
#[proc_macro_derive(Schema, attributes(schema, serde))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    schema_macro::file_cache::bump_epoch();

    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let (metadata, expanded) = schema_impl::process_derive_schema(&input);
    let Some(metadata) = metadata else {
        return TokenStream::from(expanded);
    };
    let name = metadata.name.clone();

    // Register into the current crate's bucket (see `current_crate_key`).
    // `Err` means a DIFFERENT definition is already registered under this name
    // for this crate — surface it as a hard compile error rather than the
    // silent last-write-wins overwrite that would hide a schema from the
    // generated `openapi.json`.
    if schema_impl::register_schema(name.clone(), metadata).is_err() {
        let span = input.ident.span();
        let msg = format!(
            "duplicate vespera Schema name `{name}` -- two different struct \
             definitions both register under the same OpenAPI schema name. \
             The later definition would silently overwrite the earlier one \
             in the generated `openapi.json`. Rename one of the structs, or \
             annotate one with `#[schema(name = \"OtherName\")]` to give \
             them distinct OpenAPI names."
        );
        let err = syn::Error::new(span, msg).to_compile_error();
        return TokenStream::from(err);
    }

    TokenStream::from(expanded)
}

/// Derive macro for `Multipart` with serde attribute support.
///
/// This is vespera's re-implementation of `axum_typed_multipart`'s derive macro
/// that natively supports `#[serde(rename_all)]` and `#[serde(rename)]` for
/// field name resolution in multipart form data.
///
/// # Supported Attributes
///
/// **Struct-level:**
/// - `#[serde(rename_all = "camelCase")]` — rename all fields (highest priority)
/// - `#[try_from_multipart(rename_all = "camelCase")]` — fallback rename
/// - `#[try_from_multipart(strict)]` — reject unknown/duplicate fields
///
/// **Field-level:**
/// - `#[form_data(field_name = "...")]` — explicit field name override
/// - `#[serde(rename = "...")]` — serde field rename
/// - `#[form_data(limit = "10MiB")]` — field size limit
/// - `#[form_data(default)]` — use `Default::default()` when missing
#[cfg(not(tarpaulin_include))]
#[proc_macro_derive(Multipart, attributes(serde, form_data, try_from_multipart))]
pub fn derive_multipart(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    TokenStream::from(multipart_impl::process_derive(&input))
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
    schema_macro::file_cache::bump_epoch();

    let input = syn::parse_macro_input!(input as schema_macro::SchemaInput);

    let storage = schema_impl::current_crate_schemas();

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
    schema_macro::file_cache::bump_epoch();

    let input = syn::parse_macro_input!(input as schema_macro::SchemaTypeInput);
    let ignore_schema = input.ignore_schema;

    let (tokens, generated_metadata) = {
        let storage = schema_impl::current_crate_schemas();
        match schema_macro::generate_schema_type_code(&input, &storage) {
            Ok(result) => result,
            Err(e) => return e.to_compile_error().into(),
        }
    };

    // The emitted token stream contains a struct with
    // `#[derive(Schema)]`; that derive macro registers the schema into
    // `SCHEMA_STORAGE` on its own.  We only need to pre-register here
    // when `ignore_schema` is set, because in that case the emitted
    // struct does NOT carry `#[derive(Schema)]` and would otherwise
    // be invisible to the OpenAPI generator.
    //
    // Pre-registering in the non-ignore path would cause the
    // duplicate-name check in `derive_schema` to fire on every
    // `schema_type!` call — the macro's own pre-insert collides with
    // the derive's later insert because the two `StructMetadata`
    // definitions are textually different (the pre-registered one is
    // synthesised by `schema_macro`; the derive-emitted one is the
    // expanded struct token stream).
    if ignore_schema && let Some(metadata) = generated_metadata {
        let name = metadata.name.clone();
        schema_impl::insert_schema(name, metadata);
    }
    TokenStream::from(tokens)
}

#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    schema_macro::file_cache::bump_epoch();

    let input = syn::parse_macro_input!(input as AutoRouterInput);
    // Capture the `dir = "..."` literal span (or the macro call site when
    // `dir` is omitted) before `process_vespera_input` consumes `input`, so a
    // "route folder not found" diagnostic points at the offending argument
    // rather than the whole `vespera!` invocation.
    let folder_span = input
        .dir
        .as_ref()
        .map_or_else(proc_macro2::Span::call_site, syn::LitStr::span);
    let processed = process_vespera_input(input);
    // Per-crate snapshots (see `schema_impl::current_crate_key`): a shared
    // rust-analyzer proc-macro server never leaks another crate's schemas /
    // routes into this `vespera!` expansion.
    let schema_storage = schema_impl::current_crate_schemas();
    let route_storage = route_impl::current_crate_routes();

    match process_vespera_macro(&processed, &schema_storage, &route_storage, folder_span) {
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
    schema_macro::file_cache::bump_epoch();

    let ExportAppInput { name, dir } = syn::parse_macro_input!(input as ExportAppInput);
    // Capture the `dir = "..."` literal span (or the macro call site when
    // `dir` is omitted) before `dir` is consumed below, so a "route folder
    // not found" diagnostic points at the offending argument.
    let folder_span = dir
        .as_ref()
        .map_or_else(proc_macro2::Span::call_site, syn::LitStr::span);
    let folder_name = dir
        .map(|d| d.value())
        .or_else(|| std::env::var("VESPERA_DIR").ok())
        .unwrap_or_else(|| "routes".to_string());
    let schema_storage = schema_impl::current_crate_schemas();
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return syn::Error::new(proc_macro2::Span::call_site(), "export_app! macro: CARGO_MANIFEST_DIR is not set. This macro must be used within a cargo build.").to_compile_error().into();
    };

    let route_storage = route_impl::current_crate_routes();

    match process_export_app(
        &name,
        &folder_name,
        &schema_storage,
        &manifest_dir,
        &route_storage,
        folder_span,
    ) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
