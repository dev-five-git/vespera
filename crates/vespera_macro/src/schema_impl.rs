//! Schema derive macro implementation.
//!
//! This module implements the `#[derive(Schema)]` derive macro that registers
//! types for `OpenAPI` schema generation.
//!
//! # Overview
//!
//! The `#[derive(Schema)]` macro registers a struct or enum for inclusion in the `OpenAPI` spec.
//! It stores metadata about the type which is later used by the `vespera!` macro to generate
//! the `OpenAPI` components/schemas section.
//!
//! # Global Schema Storage
//!
//! This module uses a global [`SCHEMA_STORAGE`] mutex to collect all schema types across
//! a crate at compile time. This is necessary because proc-macros are invoked independently,
//! so we need a shared location to gather all types before generating the final `OpenAPI` spec.
//!
//! # Custom Schema Names
//!
//! By default, the `OpenAPI` schema name matches the struct name. You can customize it:
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

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use crate::metadata::StructMetadata;

pub static SCHEMA_STORAGE: LazyLock<Mutex<HashMap<String, StructMetadata>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Extract custom schema name from #[schema(name = "...")] attribute
pub fn extract_schema_name_attr(attrs: &[syn::Attribute]) -> Option<String> {
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
pub fn process_derive_schema(
    input: &syn::DeriveInput,
) -> (StructMetadata, proc_macro2::TokenStream) {
    let name = &input.ident;

    // Check for custom schema name from #[schema(name = "...")] attribute
    let schema_name = extract_schema_name_attr(&input.attrs).unwrap_or_else(|| name.to_string());

    // Schema-derived types appear in OpenAPI spec (include_in_openapi: true)
    let metadata = StructMetadata::new(schema_name, quote::quote!(#input).to_string());
    (metadata, proc_macro2::TokenStream::new())
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
        let (metadata, _expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "User");
        assert!(metadata.definition.contains("struct User"));
    }

    #[test]
    fn test_process_derive_schema_enum() {
        let input: syn::DeriveInput = syn::parse_quote! {
            enum Status {
                Active,
                Inactive,
            }
        };
        let (metadata, _expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Status");
        assert!(metadata.definition.contains("enum Status"));
    }

    #[test]
    fn test_process_derive_schema_generic() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Container<T> {
                value: T,
            }
        };
        let (metadata, _expanded) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Container");
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
        let (metadata, _tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "User");
        assert!(metadata.definition.contains("User"));
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
        let (metadata, _tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Container");
    }


    #[test]
    fn test_extract_schema_name_attr_non_name_meta_key() {
        // #[schema(other = "foo")] — has schema attr but no "name" key
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema(other = "foo")]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_attr_multiple_schema_attrs() {
        // Two #[schema] attrs — first one with name wins
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema(name = "First")]
            #[schema(name = "Second")]
        };
        let result = extract_schema_name_attr(&attrs);
        assert_eq!(result, Some("First".to_string()));
    }

    #[test]
    fn test_extract_schema_name_attr_schema_with_unknown_key_value() {
        // #[schema(other = "x", name = "MyName")] — parse_nested_meta bails on unhandled
        // key=value (other = "x") since the value isn't consumed. Error is silently ignored.
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema(other = "x", name = "MyName")]
        };
        let result = extract_schema_name_attr(&attrs);
        // parse_nested_meta fails at `other = "x"` (value not consumed), so `name` is never reached
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_attr_name_before_unknown() {
        // name comes FIRST, so it's extracted before the unknown key causes a bail
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[schema(name = "Found", other = "x")]
        };
        let result = extract_schema_name_attr(&attrs);
        // name is parsed successfully; parse_nested_meta may error on `other` but name is already set
        assert_eq!(result, Some("Found".to_string()));
    }

    // ========== Coverage: process_derive_schema struct variants ==========

    #[test]
    fn test_process_derive_schema_unit_struct() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Unit;
        };
        let (metadata, tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Unit");
        assert!(metadata.definition.contains("Unit"));
        assert!(tokens.is_empty(), "Token stream should be empty");
    }

    #[test]
    fn test_process_derive_schema_tuple_struct() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Pair(i32, String);
        };
        let (metadata, tokens) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Pair");
        assert!(metadata.definition.contains("Pair"));
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_process_derive_schema_empty_struct() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Empty {}
        };
        let (metadata, _) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Empty");
    }

    #[test]
    fn test_process_derive_schema_with_lifetime() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Ref<'a> {
                data: &'a str,
            }
        };
        let (metadata, _) = process_derive_schema(&input);
        assert_eq!(metadata.name, "Ref");
    }

    #[test]
    fn test_process_derive_schema_with_serde_attrs() {
        let input: syn::DeriveInput = syn::parse_quote! {
            #[serde(rename_all = "camelCase")]
            struct UserResponse {
                user_name: String,
                #[serde(skip)]
                internal_id: u64,
            }
        };
        let (metadata, _) = process_derive_schema(&input);
        assert_eq!(metadata.name, "UserResponse");
        assert!(metadata.definition.contains("camelCase"));
        assert!(metadata.definition.contains("skip"));
    }

    // ========== Coverage: metadata field verification ==========

    #[test]
    fn test_process_derive_schema_include_in_openapi_true() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct Visible { x: i32 }
        };
        let (metadata, _) = process_derive_schema(&input);
        assert!(
            metadata.include_in_openapi,
            "Schema-derived types must have include_in_openapi=true"
        );
    }

    #[test]
    fn test_process_derive_schema_definition_contains_fields() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct WithFields {
                id: u64,
                name: String,
                active: bool,
            }
        };
        let (metadata, _) = process_derive_schema(&input);
        assert!(metadata.definition.contains("id"));
        assert!(metadata.definition.contains("u64"));
        assert!(metadata.definition.contains("name"));
        assert!(metadata.definition.contains("active"));
        assert!(metadata.definition.contains("bool"));
    }

    // ========== Coverage: SCHEMA_STORAGE direct usage ==========

    #[test]
    fn test_schema_storage_insert_and_get() {
        let storage = SCHEMA_STORAGE.lock().unwrap();
        let key = "__test_coverage_type__".to_string();
        // Clean up if previous test left data
        drop(storage);

        {
            let mut storage = SCHEMA_STORAGE.lock().unwrap();
            storage.insert(
                key.clone(),
                StructMetadata::new(key.clone(), "struct __test_coverage_type__ {}".to_string()),
            );
        }

        {
            let storage = SCHEMA_STORAGE.lock().unwrap();
            let meta = storage.get(&key);
            assert!(meta.is_some(), "Inserted metadata should be retrievable");
            let meta = meta.unwrap();
            assert_eq!(meta.name, key);
            assert!(meta.include_in_openapi);
        }

        // Cleanup
        {
            let mut storage = SCHEMA_STORAGE.lock().unwrap();
            storage.remove(&key);
        }
    }

    #[test]
    fn test_schema_storage_overwrite() {
        let key = "__test_overwrite_type__".to_string();
        {
            let mut storage = SCHEMA_STORAGE.lock().unwrap();
            storage.insert(
                key.clone(),
                StructMetadata::new(key.clone(), "struct V1 {}".to_string()),
            );
            storage.insert(
                key.clone(),
                StructMetadata::new(key.clone(), "struct V2 {}".to_string()),
            );
        }
        {
            let storage = SCHEMA_STORAGE.lock().unwrap();
            let meta = storage.get(&key).unwrap();
            assert!(meta.definition.contains("V2"), "Last insert should win");
        }
        // Cleanup
        {
            let mut storage = SCHEMA_STORAGE.lock().unwrap();
            storage.remove(&key);
        }
    }
}
