# Split lib.rs into Focused Modules Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Split `lib.rs` (~2,799 lines) into 4 focused modules, keeping only `#[proc_macro*]` entry points in `lib.rs`.

**Architecture:** Extract logical components into separate modules while maintaining the same public API. Each module will be `pub(crate)` internally and tests will move with their functions.

**Tech Stack:** Rust proc-macro crate, syn, quote, proc_macro2

---

## Current Structure Analysis

| Component | Lines | Target Module |
|-----------|-------|---------------|
| Route validation/processing | 32-70 | `route_impl.rs` |
| Schema storage & processing | 72-261 | `schema_impl.rs` |
| Input parsing (AutoRouterInput, ServerConfig) | 263-568 | `router_codegen.rs` |
| Router code generation | 570-922 | `router_codegen.rs` |
| Export app | 924-1075 | `vespera_impl.rs` |
| Vespera macro orchestration | 683-770 | `vespera_impl.rs` |
| Tests | 1077-2799 | Move with functions |

---

## Task 1: Create `src/route_impl.rs`

**Files:**
- Create: `crates/vespera_macro/src/route_impl.rs`
- Modify: `crates/vespera_macro/src/lib.rs`

**Step 1: Create route_impl.rs with functions**

```rust
//! Route attribute implementation

use crate::args;

/// Validate route function - must be pub and async
pub(crate) fn validate_route_fn(item_fn: &syn::ItemFn) -> Result<(), syn::Error> {
    if !matches!(item_fn.vis, syn::Visibility::Public(_)) {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "route function must be public",
        ));
    }
    if item_fn.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "route function must be async",
        ));
    }
    Ok(())
}

/// Process route attribute - extracted for testability
pub(crate) fn process_route_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    syn::parse2::<args::RouteArgs>(attr)?;
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| {
        syn::Error::new(e.span(), "route attribute can only be applied to functions")
    })?;
    validate_route_fn(&item_fn)?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn test_validate_route_fn_not_public() {
        let item: syn::ItemFn = syn::parse_quote! {
            async fn private_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be public"));
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
        assert!(result.unwrap_err().to_string().contains("must be async"));
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
        assert!(err.contains("must be public"));
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
        assert!(err.contains("must be async"));
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
            assert!(result.is_ok(), "Method {} should be valid", method);
        }
    }
}
```

**Step 2: Run tests to verify extraction**

Run: `cargo test -p vespera_macro`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/vespera_macro/src/route_impl.rs
git commit -m "refactor(vespera_macro): extract route_impl.rs module"
```

---

## Task 2: Create `src/schema_impl.rs`

**Files:**
- Create: `crates/vespera_macro/src/schema_impl.rs`
- Modify: `crates/vespera_macro/src/lib.rs`

**Step 1: Create schema_impl.rs with functions**

```rust
//! Schema derive implementation

use std::sync::{LazyLock, Mutex};

use quote::quote;

use crate::metadata::StructMetadata;

#[cfg(not(tarpaulin_include))]
pub(crate) fn init_schema_storage() -> Mutex<Vec<StructMetadata>> {
    Mutex::new(Vec::new())
}

pub(crate) static SCHEMA_STORAGE: LazyLock<Mutex<Vec<StructMetadata>>> =
    LazyLock::new(init_schema_storage);

/// Extract custom schema name from #[schema(name = "...")] attribute
pub(crate) fn extract_schema_name_attr(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("schema") {
            let mut custom_name = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    custom_name = Some(lit.value());
                }
                Ok(())
            });
            if custom_name.is_some() {
                return custom_name;
            }
        }
    }
    None
}

/// Process derive input and return metadata + expanded code
pub(crate) fn process_derive_schema(
    input: &syn::DeriveInput,
) -> (StructMetadata, proc_macro2::TokenStream) {
    let name = &input.ident;
    let generics = &input.generics;

    // Check for custom schema name from #[schema(name = "...")] attribute
    let schema_name = extract_schema_name_attr(&input.attrs).unwrap_or_else(|| name.to_string());

    // Schema-derived types appear in OpenAPI spec (include_in_openapi: true)
    let metadata = StructMetadata::new(schema_name, quote!(#input).to_string());
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let expanded = quote! {
        impl #impl_generics vespera::schema::SchemaBuilder for #name #ty_generics #where_clause {}
    };
    (metadata, expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_schema_name_attr_with_name() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema(name = "CustomName")]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, Some("CustomName".to_string()));
    }

    #[test]
    fn test_extract_schema_name_attr_without_name() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[derive(Debug)]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_attr_empty_schema() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_attr_with_other_attrs() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[derive(Clone)]
            #[schema(name = "MySchema")]
            #[serde(rename_all = "camelCase")]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, Some("MySchema".to_string()));
    }

    #[test]
    fn test_process_derive_schema_struct() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct User {
                name: String,
                age: u32,
            }
        };
        let (metadata, expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "User");
        assert!(metadata.definition.contains("struct User"));
        let code = expanded.to_string();
        assert!(code.contains("SchemaBuilder"));
        assert!(code.contains("User"));
    }

    #[test]
    fn test_process_derive_schema_enum() {
        let input: syn::DeriveInput = syn::parse_quote! {
            enum Status {
                Active,
                Inactive,
            }
        };
        let (metadata, expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Status");
        assert!(metadata.definition.contains("enum Status"));
        let code = expanded.to_string();
        assert!(code.contains("SchemaBuilder"));
    }

    #[test]
    fn test_process_derive_schema_generic() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Container<T> {
                value: T,
            }
        };
        let (metadata, expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Container");
        let code = expanded.to_string();
        assert!(code.contains("SchemaBuilder"));
        assert!(code.contains("impl"));
    }

    #[test]
    fn test_process_derive_schema_simple() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct User {
                id: i32,
                name: String,
            }
        };
        let (metadata, tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "User");
        assert!(metadata.definition.contains("User"));
        let tokens_str = tokens.to_string();
        assert!(tokens_str.contains("SchemaBuilder"));
    }

    #[test]
    fn test_process_derive_schema_with_custom_name() {
        let input: syn::DeriveInput = syn::parse_quote! {
            #[schema(name = "CustomUserSchema")]
            struct User {
                id: i32,
            }
        };
        let (metadata, _) = process_derive_schema(&input);
        assert_eq!(metadata.name, "CustomUserSchema");
    }

    #[test]
    fn test_process_derive_schema_with_generics() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Container<T> {
                value: T,
            }
        };
        let (metadata, tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Container");
        let tokens_str = tokens.to_string();
        assert!(tokens_str.contains("< T >") || tokens_str.contains("<T>"));
    }
}
```

**Step 2: Run tests to verify extraction**

Run: `cargo test -p vespera_macro`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/vespera_macro/src/schema_impl.rs
git commit -m "refactor(vespera_macro): extract schema_impl.rs module"
```

---

## Task 3: Create `src/router_codegen.rs`

**Files:**
- Create: `crates/vespera_macro/src/router_codegen.rs`
- Modify: `crates/vespera_macro/src/lib.rs`

**Step 1: Create router_codegen.rs with structs and parsing functions**

This file will contain:
- `ServerConfig` struct
- `AutoRouterInput` struct + Parse impl
- `ExportAppInput` struct + Parse impl
- `ProcessedVesperaInput` struct
- All parsing helper functions
- `process_vespera_input()` function
- `generate_router_code()` function
- Related tests

The file is large (~800 lines with tests), so extract all these components:

```rust
//! Router code generation and input parsing

use proc_macro2::Span;
use quote::quote;
use syn::bracketed;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::LitStr;

use crate::collector::collect_metadata;
use crate::metadata::CollectedMetadata;
use crate::method::http_method_to_token_stream;
use vespera_core::openapi::Server;
use vespera_core::route::HttpMethod;

/// Server configuration for OpenAPI
#[derive(Clone)]
pub(crate) struct ServerConfig {
    pub url: String,
    pub description: Option<String>,
}

pub(crate) struct AutoRouterInput {
    pub dir: Option<LitStr>,
    pub openapi: Option<Vec<LitStr>>,
    pub title: Option<LitStr>,
    pub version: Option<LitStr>,
    pub docs_url: Option<LitStr>,
    pub redoc_url: Option<LitStr>,
    pub servers: Option<Vec<ServerConfig>>,
    pub merge: Option<Vec<syn::Path>>,
}

// ... [Parse impl for AutoRouterInput - copy from lib.rs lines 282-405]

/// Input for export_app! macro
pub(crate) struct ExportAppInput {
    pub name: syn::Ident,
    pub dir: Option<LitStr>,
}

// ... [Parse impl for ExportAppInput - copy from lib.rs lines 932-965]

/// Processed vespera input with extracted values
pub(crate) struct ProcessedVesperaInput {
    pub folder_name: String,
    pub openapi_file_names: Vec<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub docs_url: Option<String>,
    pub redoc_url: Option<String>,
    pub servers: Option<Vec<Server>>,
    pub merge: Vec<syn::Path>,
}

// ... [All helper functions: parse_merge_values, parse_openapi_values, validate_server_url, parse_servers_values, parse_server_struct]
// ... [process_vespera_input function]
// ... [generate_router_code function]
// ... [All related tests]
```

**Step 2: Run tests to verify extraction**

Run: `cargo test -p vespera_macro`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/vespera_macro/src/router_codegen.rs
git commit -m "refactor(vespera_macro): extract router_codegen.rs module"
```

---

## Task 4: Create `src/vespera_impl.rs`

**Files:**
- Create: `crates/vespera_macro/src/vespera_impl.rs`
- Modify: `crates/vespera_macro/src/lib.rs`

**Step 1: Create vespera_impl.rs with orchestration functions**

```rust
//! Main vespera!() macro orchestration

use std::path::Path;

use proc_macro2::Span;

use crate::collector::collect_metadata;
use crate::error::{err_call_site, MacroResult};
use crate::metadata::{CollectedMetadata, StructMetadata};
use crate::openapi_generator::generate_openapi_doc_with_metadata;
use crate::router_codegen::{generate_router_code, ProcessedVesperaInput};

/// Docs info tuple type alias for cleaner signatures
pub(crate) type DocsInfo = (Option<(String, String)>, Option<(String, String)>);

/// Generate OpenAPI JSON and write to files, returning docs info
pub(crate) fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
) -> MacroResult<DocsInfo> {
    // ... [copy from lib.rs lines 617-681]
}

/// Process vespera macro - extracted for testability
pub(crate) fn process_vespera_macro(
    processed: &ProcessedVesperaInput,
    schema_storage: &[StructMetadata],
) -> syn::Result<proc_macro2::TokenStream> {
    // ... [copy from lib.rs lines 684-712]
}

pub(crate) fn find_folder_path(folder_name: &str) -> std::path::PathBuf {
    // ... [copy from lib.rs lines 727-737]
}

/// Find the workspace root's target directory
pub(crate) fn find_target_dir(manifest_path: &Path) -> std::path::PathBuf {
    // ... [copy from lib.rs lines 740-770]
}

/// Process export_app macro - extracted for testability
pub(crate) fn process_export_app(
    name: &syn::Ident,
    folder_name: &str,
    schema_storage: &[StructMetadata],
    manifest_dir: &str,
) -> syn::Result<proc_macro2::TokenStream> {
    // ... [copy from lib.rs lines 990-1058]
}

#[cfg(test)]
mod tests {
    // ... [All related tests for these functions]
}
```

**Step 2: Run tests to verify extraction**

Run: `cargo test -p vespera_macro`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/vespera_macro/src/vespera_impl.rs
git commit -m "refactor(vespera_macro): extract vespera_impl.rs module"
```

---

## Task 5: Simplify lib.rs

**Files:**
- Modify: `crates/vespera_macro/src/lib.rs`

**Step 1: Replace lib.rs content with minimal entry points**

```rust
//! Vespera proc-macro crate
//!
//! This crate provides the procedural macros for Vespera:
//! - `#[route]` - Route attribute for handler functions
//! - `#[derive(Schema)]` - Schema derivation for types
//! - `vespera!` - Main macro for router generation
//! - `schema!` - Runtime schema access
//! - `schema_type!` - Type generation from schemas
//! - `export_app!` - Export app for merging

mod args;
mod collector;
mod error;
mod file_utils;
mod http;
mod metadata;
mod method;
mod openapi_generator;
mod parser;
mod route;
mod route_impl;
mod router_codegen;
mod schema_impl;
mod schema_macro;
mod vespera_impl;

pub(crate) use error::{err_call_site, MacroResult};

use proc_macro::TokenStream;

use crate::router_codegen::{AutoRouterInput, ExportAppInput};
use crate::schema_impl::SCHEMA_STORAGE;

/// Route attribute macro
///
/// Validates that the function is `pub async fn` and processes route attributes.
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
/// Supports `#[schema(name = "CustomName")]` attribute to set custom OpenAPI schema name.
#[cfg(not(tarpaulin_include))]
#[proc_macro_derive(Schema, attributes(schema))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let (metadata, expanded) = schema_impl::process_derive_schema(&input);
    SCHEMA_STORAGE.lock().unwrap().push(metadata);
    TokenStream::from(expanded)
}

/// Generate an OpenAPI Schema from a type with optional field filtering.
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as schema_macro::SchemaInput);
    let storage = SCHEMA_STORAGE.lock().unwrap();

    match schema_macro::generate_schema_code(&input, &storage) {
        Ok(tokens) => TokenStream::from(tokens),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Generate a new struct type derived from an existing type with field filtering.
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn schema_type(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as schema_macro::SchemaTypeInput);
    let mut storage = SCHEMA_STORAGE.lock().unwrap();

    match schema_macro::generate_schema_type_code(&input, &storage) {
        Ok((tokens, generated_metadata)) => {
            if let Some(metadata) = generated_metadata {
                storage.push(metadata);
            }
            TokenStream::from(tokens)
        }
        Err(e) => e.to_compile_error().into(),
    }
}

/// Main vespera macro for router generation
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as AutoRouterInput);
    let processed = router_codegen::process_vespera_input(input);
    let schema_storage = SCHEMA_STORAGE.lock().unwrap();

    match vespera_impl::process_vespera_macro(&processed, &schema_storage) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Export a vespera app as a reusable component.
#[cfg(not(tarpaulin_include))]
#[proc_macro]
pub fn export_app(input: TokenStream) -> TokenStream {
    let ExportAppInput { name, dir } = syn::parse_macro_input!(input as ExportAppInput);
    let folder_name = dir
        .map(|d| d.value())
        .or_else(|| std::env::var("VESPERA_DIR").ok())
        .unwrap_or_else(|| "routes".to_string());
    let schema_storage = SCHEMA_STORAGE.lock().unwrap();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");

    match vespera_impl::process_export_app(&name, &folder_name, &schema_storage, &manifest_dir) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
```

**Step 2: Run all tests to verify refactoring**

Run: `cargo test --workspace`
Expected: All tests pass

**Step 3: Run clippy to verify no warnings**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

**Step 4: Commit**

```bash
git add crates/vespera_macro/src/lib.rs
git commit -m "refactor(vespera_macro): simplify lib.rs to entry points only"
```

---

## Task 6: Final Verification

**Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass

**Step 2: Run clippy with pedantic**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

**Step 3: Build the example to verify macros work**

Run: `cargo build -p axum-example`
Expected: Build succeeds

**Step 4: Final commit (if any fixes needed)**

```bash
git add .
git commit -m "refactor(vespera_macro): complete lib.rs module split"
```

---

## Summary of Expected Files

After completion:

| File | Purpose | Approx Lines |
|------|---------|--------------|
| `lib.rs` | Macro entry points only | ~100 |
| `route_impl.rs` | Route validation & processing | ~160 |
| `schema_impl.rs` | Schema storage & derive | ~150 |
| `router_codegen.rs` | Input parsing & router generation | ~800 |
| `vespera_impl.rs` | Orchestration & file operations | ~400 |

**Total: ~1610 lines (excluding existing modules)**

Note: The tests significantly increase line counts. The actual business logic is:
- `route_impl.rs`: ~30 lines
- `schema_impl.rs`: ~50 lines  
- `router_codegen.rs`: ~400 lines
- `vespera_impl.rs`: ~200 lines
