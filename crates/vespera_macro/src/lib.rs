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
        impl vespera::schema::SchemaBuilder for #name {}
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

    generate_router_code(&folder_path, &folder_name).into()
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

fn generate_router_code(folder_path: &Path, folder_name: &str) -> proc_macro2::TokenStream {
    // Collect metadata (routes and structs) - used by vespera_openapi!() as well
    let metadata = match collect_metadata(folder_path, folder_name) {
        Ok(metadata) => metadata,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to collect metadata: {}", e),
            )
            .to_compile_error();
        }
    };

    let mut router_nests = Vec::new();

    for route in metadata.routes {
        let http_method = HttpMethod::from(route.method.as_str());
        let method_path = http_method_to_token_stream(http_method);
        let path = route.path;
        let module_path = route.module_path;
        let function_name = route.function_name;

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

    quote! {
        vespera::axum::Router::new()
            #( #router_nests )*
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    #[test]
    fn test_generate_router_code_empty() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Should generate empty router
        // quote! generates "vespera :: axum :: Router :: new ()" format
        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {}",
            code
        );
        assert!(
            !code.contains("route"),
            "Code should not contain route, got: {}",
            code
        );

        drop(temp_dir);
    }

    #[rstest]
    #[case::single_get_route(
        "routes",
        vec![(
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/users",
        "routes::users::get_users",
    )]
    #[case::single_post_route(
        "routes",
        vec![(
            "create_user.rs",
            r#"
#[route(post)]
pub fn create_user() -> String {
    "created".to_string()
}
"#,
        )],
        "post",
        "/create_user",
        "routes::create_user::create_user",
    )]
    #[case::single_put_route(
        "routes",
        vec![(
            "update_user.rs",
            r#"
#[route(put)]
pub fn update_user() -> String {
    "updated".to_string()
}
"#,
        )],
        "put",
        "/update_user",
        "routes::update_user::update_user",
    )]
    #[case::single_delete_route(
        "routes",
        vec![(
            "delete_user.rs",
            r#"
#[route(delete)]
pub fn delete_user() -> String {
    "deleted".to_string()
}
"#,
        )],
        "delete",
        "/delete_user",
        "routes::delete_user::delete_user",
    )]
    #[case::single_patch_route(
        "routes",
        vec![(
            "patch_user.rs",
            r#"
#[route(patch)]
pub fn patch_user() -> String {
    "patched".to_string()
}
"#,
        )],
        "patch",
        "/patch_user",
        "routes::patch_user::patch_user",
    )]
    #[case::route_with_custom_path(
        "routes",
        vec![(
            "users.rs",
            r#"
#[route(get, path = "/api/users")]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/users/api/users",
        "routes::users::get_users",
    )]
    #[case::nested_module(
        "routes",
        vec![(
            "api/users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/api/users",
        "routes::api::users::get_users",
    )]
    #[case::deeply_nested_module(
        "routes",
        vec![(
            "api/v1/users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/api/v1/users",
        "routes::api::v1::users::get_users",
    )]
    fn test_generate_router_code_single_route(
        #[case] folder_name: &str,
        #[case] files: Vec<(&str, &str)>,
        #[case] expected_method: &str,
        #[case] expected_path: &str,
        #[case] expected_function_path: &str,
    ) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        for (filename, content) in files {
            create_temp_file(&temp_dir, filename, content);
        }

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {}",
            code
        );

        // Check route method
        assert!(
            code.contains(expected_method),
            "Code should contain method: {}, got: {}",
            expected_method,
            code
        );

        // Check route path
        assert!(
            code.contains(expected_path),
            "Code should contain path: {}, got: {}",
            expected_path,
            code
        );

        // Check function path (quote! adds spaces, so we check for parts)
        let function_parts: Vec<&str> = expected_function_path.split("::").collect();
        for part in &function_parts {
            if !part.is_empty() {
                assert!(
                    code.contains(part),
                    "Code should contain function part: {}, got: {}",
                    part,
                    code
                );
            }
        }

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_multiple_routes() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create multiple route files
        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        create_temp_file(
            &temp_dir,
            "create_user.rs",
            r#"
#[route(post)]
pub fn create_user() -> String {
    "created".to_string()
}
"#,
        );

        create_temp_file(
            &temp_dir,
            "update_user.rs",
            r#"
#[route(put)]
pub fn update_user() -> String {
    "updated".to_string()
}
"#,
        );

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(code.contains("Router") && code.contains("new"));

        // Check all routes are present
        assert!(code.contains("get_users"));
        assert!(code.contains("create_user"));
        assert!(code.contains("update_user"));

        // Check methods
        assert!(code.contains("get"));
        assert!(code.contains("post"));
        assert!(code.contains("put"));

        // Count route calls (quote! generates ". route (" with spaces)
        // Count occurrences of ". route (" pattern
        let route_count = code.matches(". route (").count();
        assert_eq!(
            route_count, 3,
            "Should have 3 route calls, got: {}, code: {}",
            route_count, code
        );

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_same_path_different_methods() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create routes with same path but different methods
        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}

#[route(post)]
pub fn create_users() -> String {
    "created".to_string()
}
"#,
        );

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(code.contains("Router") && code.contains("new"));

        // Check both routes are present
        assert!(code.contains("get_users"));
        assert!(code.contains("create_users"));

        // Check methods
        assert!(code.contains("get"));
        assert!(code.contains("post"));

        // Should have 2 routes (quote! generates ". route (" with spaces)
        let route_count = code.matches(". route (").count();
        assert_eq!(
            route_count, 2,
            "Should have 2 routes, got: {}, code: {}",
            route_count, code
        );

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_with_mod_rs() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create mod.rs file
        create_temp_file(
            &temp_dir,
            "mod.rs",
            r#"
#[route(get)]
pub fn index() -> String {
    "index".to_string()
}
"#,
        );

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(code.contains("Router") && code.contains("new"));

        // Check route is present
        assert!(code.contains("index"));

        // Path should be / (mod.rs maps to root, segments is empty)
        // quote! generates "\"/\""
        assert!(code.contains("\"/\""));

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_empty_folder_name() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "";

        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        let result = generate_router_code(temp_dir.path(), folder_name);
        let code = result.to_string();

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(code.contains("Router") && code.contains("new"));

        // Check route is present
        assert!(code.contains("get_users"));

        // Module path should not have double colons
        assert!(!code.contains("::users::users"));

        drop(temp_dir);
    }
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
    let metadata = match collect_metadata(&folder_path, &folder_name) {
        Ok(metadata) => metadata,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to collect metadata: {}", e),
            )
            .to_compile_error()
            .into();
        }
    };

    // Generate OpenAPI document using collected metadata
    let openapi_doc = generate_openapi_doc_with_metadata(title, version, &metadata);

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
