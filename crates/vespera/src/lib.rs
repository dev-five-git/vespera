mod args;
mod collector;
mod file_utils;
mod metadata;
mod method;
mod openapi_generator;
mod parser;
mod route;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use std::path::Path;
use syn::LitStr;
use syn::parse::{Parse, ParseStream};

use crate::collector::collect_metadata;
use crate::method::http_method_to_token_stream;
use crate::openapi_generator::generate_openapi_doc_with_metadata;
use vespera_core::route::HttpMethod;

/// route attribute macro
#[proc_macro_attribute]
pub fn route(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Derive macro for Schema
#[proc_macro_derive(Schema)]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let name = &input.ident;

    // For now, we just mark the struct as having SchemaBuilder
    // The actual schema generation will be done at runtime
    let expanded = quote! {
        impl vespera_core::schema::SchemaBuilder for #name {}
    };

    TokenStream::from(expanded)
}

struct AutoRouterInput {
    folder: Option<LitStr>,
}

impl Parse for AutoRouterInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(AutoRouterInput { folder: None });
        }

        let folder = input.parse::<LitStr>()?;
        Ok(AutoRouterInput {
            folder: Some(folder),
        })
    }
}

#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as AutoRouterInput);

    let folder_name = input
        .folder
        .map(|f| f.value())
        .unwrap_or_else(|| "routes".to_string());

    let folder_path = find_folder_path(&folder_name);

    if !folder_path.exists() {
        return syn::Error::new(
            Span::call_site(),
            format!("Folder not found: {}", folder_name),
        )
        .to_compile_error()
        .into();
    }

    generate_router_code(&folder_path, &folder_name)
}

fn find_folder_path(folder_name: &str) -> std::path::PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let path = format!("{}/src/{}", root, folder_name);
    let path = Path::new(&path);
    if path.exists() && path.is_dir() {
        return path.to_path_buf();
    }

    Path::new(folder_name).to_path_buf()
}

fn generate_router_code(folder_path: &Path, folder_name: &str) -> TokenStream {
    // Collect metadata (routes and structs) - used by vespera_openapi!() as well
    let metadata = collect_metadata(folder_path, folder_name);

    let mut router_nests = Vec::new();

    for route in metadata.routes {
        let http_method = HttpMethod::from(route.method.as_str());
        let method_path = http_method_to_token_stream(http_method);
        let path = route.path.clone().to_string();
        let module_path = route.module_path.clone().to_string();
        let function_name = route.function_name.clone().to_string();

        let mut p: syn::punctuated::Punctuated<syn::PathSegment, syn::Token![::]> =
            syn::punctuated::Punctuated::new();
        p.push(syn::PathSegment {
            ident: syn::Ident::new("crate", Span::call_site()),
            arguments: syn::PathArguments::None,
        });
        p.extend(
            module_path
                .split("::")
                .filter_map(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(syn::PathSegment {
                            ident: syn::Ident::new(s, Span::call_site()),
                            arguments: syn::PathArguments::None,
                        })
                    }
                })
                .collect::<Vec<syn::PathSegment>>(),
        );
        let func_name = syn::Ident::new(&function_name, Span::call_site());
        router_nests.push(quote!(
            .route(#path, #method_path(#p::#func_name))
        ));
    }

    let expanded = quote! {
        axum::Router::new()
            #( #router_nests )*
    };

    expanded.into()
}

struct OpenApiInput {
    folder: Option<LitStr>,
    title: Option<LitStr>,
    version: Option<LitStr>,
}

impl Parse for OpenApiInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut folder = None;
        let mut title = None;
        let mut version = None;

        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string();

                match ident_str.as_str() {
                    "folder" => {
                        input.parse::<syn::Token![=]>()?;
                        folder = Some(input.parse()?);
                    }
                    "title" => {
                        input.parse::<syn::Token![=]>()?;
                        title = Some(input.parse()?);
                    }
                    "version" => {
                        input.parse::<syn::Token![=]>()?;
                        version = Some(input.parse()?);
                    }
                    _ => {
                        return Err(lookahead.error());
                    }
                }
            } else if lookahead.peek(syn::LitStr) {
                // If just a string, treat it as folder
                folder = Some(input.parse()?);
            } else {
                return Err(lookahead.error());
            }

            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(OpenApiInput {
            folder,
            title,
            version,
        })
    }
}

/// Generate OpenAPI JSON string from routes
#[proc_macro]
pub fn vespera_openapi(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as OpenApiInput);

    let folder_name = input
        .folder
        .as_ref()
        .map(|f| f.value())
        .unwrap_or_else(|| "routes".to_string());
    let title = input.title.map(|t| t.value());
    let version = input.version.map(|v| v.value());

    let folder_path = find_folder_path(&folder_name);

    if !folder_path.exists() {
        return syn::Error::new(
            Span::call_site(),
            format!(
                "Folder not found: {}. Make sure vespera!() uses the same folder name.",
                folder_name
            ),
        )
        .to_compile_error()
        .into();
    }

    // Collect metadata (same as vespera!() does)
    let metadata = collect_metadata(&folder_path, &folder_name);

    // Generate OpenAPI document using collected metadata
    let openapi_doc =
        generate_openapi_doc_with_metadata(&folder_path, &folder_name, title, version, &metadata);

    // Serialize to JSON
    let json_str = match serde_json::to_string_pretty(&openapi_doc) {
        Ok(json) => json,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to serialize OpenAPI document: {}", e),
            )
            .to_compile_error()
            .into();
        }
    };

    // Return as a string literal
    let expanded = quote! {
        #json_str
    };

    expanded.into()
}
