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
use std::sync::{LazyLock, Mutex};
use syn::LitStr;
use syn::parse::{Parse, ParseStream};

use crate::collector::collect_metadata;
use crate::metadata::{CollectedMetadata, StructMetadata};
use crate::method::http_method_to_token_stream;
use crate::openapi_generator::generate_openapi_doc_with_metadata;
use vespera_core::route::HttpMethod;

/// route attribute macro
#[proc_macro_attribute]
pub fn route(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

// Schema Storage global variable
static SCHEMA_STORAGE: LazyLock<Mutex<Vec<StructMetadata>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Derive macro for Schema
#[proc_macro_derive(Schema)]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let name = &input.ident;

    let mut schema_storage = SCHEMA_STORAGE.lock().unwrap();
    schema_storage.push(StructMetadata {
        name: name.to_string(),
        definition: quote::quote!(#input).to_string(),
    });

    // For now, we just mark the struct as having SchemaBuilder
    // The actual schema generation will be done at runtime
    let expanded = quote! {
        impl vespera::schema::SchemaBuilder for #name {}
    };

    TokenStream::from(expanded)
}

struct AutoRouterInput {
    dir: Option<LitStr>,
    openapi: Option<LitStr>,
    title: Option<LitStr>,
    version: Option<LitStr>,
    docs_url: Option<LitStr>,
}

impl Parse for AutoRouterInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut dir = None;
        let mut openapi = None;
        let mut title = None;
        let mut version = None;
        let mut docs_url = None;

        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string();

                match ident_str.as_str() {
                    "dir" => {
                        input.parse::<syn::Token![=]>()?;
                        dir = Some(input.parse()?);
                    }
                    "openapi" => {
                        input.parse::<syn::Token![=]>()?;
                        openapi = Some(input.parse()?);
                    }
                    "docs_url" => {
                        input.parse::<syn::Token![=]>()?;
                        docs_url = Some(input.parse()?);
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
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown field: `{}`. Expected `dir` or `openapi`",
                                ident_str
                            ),
                        ));
                    }
                }
            } else if lookahead.peek(syn::LitStr) {
                // If just a string, treat it as dir (for backward compatibility)
                dir = Some(input.parse()?);
            } else {
                return Err(lookahead.error());
            }

            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(AutoRouterInput {
            dir: dir.or_else(|| {
                std::env::var("VESPERA_DIR")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            openapi: openapi.or_else(|| {
                std::env::var("VESPERA_OPENAPI")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            title: title.or_else(|| {
                std::env::var("VESPERA_TITLE")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            version: version.or_else(|| {
                std::env::var("VESPERA_VERSION")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            docs_url: docs_url.or_else(|| {
                std::env::var("VESPERA_DOCS_URL")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
        })
    }
}

#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as AutoRouterInput);

    let folder_name = input
        .dir
        .map(|f| f.value())
        .unwrap_or_else(|| "routes".to_string());

    let openapi_file_name = input.openapi.map(|f| f.value());

    let title = input.title.map(|t| t.value());
    let version = input.version.map(|v| v.value());
    let docs_url = input.docs_url.map(|u| u.value());

    let folder_path = find_folder_path(&folder_name);

    if !folder_path.exists() {
        return syn::Error::new(
            Span::call_site(),
            format!("Folder not found: {}", folder_name),
        )
        .to_compile_error()
        .into();
    }

    let mut metadata = match collect_metadata(&folder_path, &folder_name) {
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
    let schemas = SCHEMA_STORAGE.lock().unwrap().clone();

    metadata.structs.extend(schemas);

    let mut docs_info = None;

    if openapi_file_name.is_some() || docs_url.is_some() {
        // Generate OpenAPI document using collected metadata

        // Serialize to JSON
        let json_str = match serde_json::to_string_pretty(&generate_openapi_doc_with_metadata(
            title, version, &metadata,
        )) {
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
        if let Some(openapi_file_name) = openapi_file_name {
            std::fs::write(openapi_file_name, &json_str).unwrap();
        }
        if let Some(docs_url) = docs_url {
            docs_info = Some((docs_url, json_str));
        }
    }

    generate_router_code(&metadata, docs_info).into()
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

fn generate_router_code(
    metadata: &CollectedMetadata,
    docs_info: Option<(String, String)>,
) -> proc_macro2::TokenStream {
    let mut router_nests = Vec::new();

    for route in &metadata.routes {
        let http_method = HttpMethod::from(route.method.as_str());
        let method_path = http_method_to_token_stream(http_method);
        let path = &route.path;
        let module_path = &route.module_path;
        let function_name = &route.function_name;

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

    if let Some((docs_url, spec)) = docs_info {
        let method_path = http_method_to_token_stream(HttpMethod::Get);

        let html = format!(
            r#"
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Swagger UI</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist/swagger-ui.css" />
</head>
<body style="margin: 0; padding: 0;">
<div id="swagger-ui"></div>

<script src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js"></script>
<script src="https://unpkg.com/swagger-ui-dist/swagger-ui-standalone-preset.js"></script>

<script>
  const openapiSpec = {spec_json};

  window.onload = () => {{
    SwaggerUIBundle({{
      spec: openapiSpec,
      dom_id: "\#swagger-ui",
      presets: [
        SwaggerUIBundle.presets.apis,
        SwaggerUIStandalonePreset
      ],
      layout: "StandaloneLayout"
    }});
  }};
</script>

</body>
</html>
"#,
            spec_json = spec
        )
        .replace("\n", "");

        router_nests.push(quote!(
            .route(#docs_url, #method_path(|| async { vespera::axum::response::Html(#html) }))
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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

        let result = generate_router_code(
            &collect_metadata(&temp_dir.path(), folder_name).unwrap(),
            None,
        );
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
