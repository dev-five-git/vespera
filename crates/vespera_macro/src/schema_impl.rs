//! Schema derive macro implementation.
//!
//! This module implements the `#[derive(Schema)]` derive macro that registers
//! types for OpenAPI schema generation.
//!
//! # Overview
//!
//! The `#[derive(Schema)]` macro registers a struct or enum for inclusion in the OpenAPI spec.
//! It stores metadata about the type which is later used by the `vespera!` macro to generate
//! the OpenAPI components/schemas section.
//!
//! # Global Schema Storage
//!
//! This module uses a global [`SCHEMA_STORAGE`] mutex to collect all schema types across
//! a crate at compile time. This is necessary because proc-macros are invoked independently,
//! so we need a shared location to gather all types before generating the final OpenAPI spec.
//!
//! # Custom Schema Names
//!
//! By default, the OpenAPI schema name matches the struct name. You can customize it:
//!
//! ```ignore
//! #[derive(Schema)]
//! #[schema(name = "CustomSchemaName")]
//! pub struct MyType { ... }
//! ```
//!
//! # Key Functions
//!
//! - [`extract_schema_name_attr`] - Extract custom name from `#[schema]` attribute
//! - [`process_derive_schema`] - Process the derive macro input and register the type

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
    let metadata = StructMetadata::new(schema_name, quote::quote!(#input).to_string());
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
        // Should have generic impl
        assert!(code.contains("impl"));
    }

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
