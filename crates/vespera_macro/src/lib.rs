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
use syn::bracketed;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;

use crate::collector::collect_metadata;
use crate::metadata::{CollectedMetadata, StructMetadata};
use crate::method::http_method_to_token_stream;
use crate::openapi_generator::generate_openapi_doc_with_metadata;
use vespera_core::openapi::Server;
use vespera_core::route::HttpMethod;

/// Validate route function - must be pub and async
fn validate_route_fn(item_fn: &syn::ItemFn) -> Result<(), syn::Error> {
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

/// route attribute macro
#[proc_macro_attribute]
pub fn route(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Err(e) = syn::parse::<args::RouteArgs>(attr) {
        return e.to_compile_error().into();
    }
    let item_fn = match syn::parse::<syn::ItemFn>(item.clone()) {
        Ok(f) => f,
        Err(e) => {
            return syn::Error::new(e.span(), "route attribute can only be applied to functions")
                .to_compile_error()
                .into();
        }
    };
    if let Err(e) = validate_route_fn(&item_fn) {
        return e.to_compile_error().into();
    }
    item
}

// Schema Storage global variable
static SCHEMA_STORAGE: LazyLock<Mutex<Vec<StructMetadata>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Process derive input and return metadata + expanded code
fn process_derive_schema(input: &syn::DeriveInput) -> (StructMetadata, proc_macro2::TokenStream) {
    let name = &input.ident;
    let generics = &input.generics;
    let metadata = StructMetadata {
        name: name.to_string(),
        definition: quote::quote!(#input).to_string(),
    };
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let expanded = quote! {
        impl #impl_generics vespera::schema::SchemaBuilder for #name #ty_generics #where_clause {}
    };
    (metadata, expanded)
}

/// Derive macro for Schema
#[proc_macro_derive(Schema)]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let (metadata, expanded) = process_derive_schema(&input);
    SCHEMA_STORAGE.lock().unwrap().push(metadata);
    TokenStream::from(expanded)
}

/// Server configuration for OpenAPI
#[derive(Clone)]
struct ServerConfig {
    url: String,
    description: Option<String>,
}

struct AutoRouterInput {
    dir: Option<LitStr>,
    openapi: Option<Vec<LitStr>>,
    title: Option<LitStr>,
    version: Option<LitStr>,
    docs_url: Option<LitStr>,
    redoc_url: Option<LitStr>,
    servers: Option<Vec<ServerConfig>>,
    /// Apps to merge (e.g., [third::ThirdApp, another::AnotherApp])
    merge: Option<Vec<syn::Path>>,
}

impl Parse for AutoRouterInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut dir = None;
        let mut openapi = None;
        let mut title = None;
        let mut version = None;
        let mut docs_url = None;
        let mut redoc_url = None;
        let mut servers = None;
        let mut merge = None;

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
                        openapi = Some(parse_openapi_values(input)?);
                    }
                    "docs_url" => {
                        input.parse::<syn::Token![=]>()?;
                        docs_url = Some(input.parse()?);
                    }
                    "redoc_url" => {
                        input.parse::<syn::Token![=]>()?;
                        redoc_url = Some(input.parse()?);
                    }
                    "title" => {
                        input.parse::<syn::Token![=]>()?;
                        title = Some(input.parse()?);
                    }
                    "version" => {
                        input.parse::<syn::Token![=]>()?;
                        version = Some(input.parse()?);
                    }
                    "servers" => {
                        servers = Some(parse_servers_values(input)?);
                    }
                    "merge" => {
                        merge = Some(parse_merge_values(input)?);
                    }
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown field: `{}`. Expected `dir`, `openapi`, `title`, `version`, `docs_url`, `redoc_url`, `servers`, or `merge`",
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
                    .map(|f| vec![LitStr::new(&f, Span::call_site())])
                    .ok()
            }),
            title: title.or_else(|| {
                std::env::var("VESPERA_TITLE")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            version: version
                .or_else(|| {
                    std::env::var("VESPERA_VERSION")
                        .map(|f| LitStr::new(&f, Span::call_site()))
                        .ok()
                })
                .or_else(|| {
                    std::env::var("CARGO_PKG_VERSION")
                        .map(|f| LitStr::new(&f, Span::call_site()))
                        .ok()
                }),
            docs_url: docs_url.or_else(|| {
                std::env::var("VESPERA_DOCS_URL")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            redoc_url: redoc_url.or_else(|| {
                std::env::var("VESPERA_REDOC_URL")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            servers: servers.or_else(|| {
                std::env::var("VESPERA_SERVER_URL")
                    .ok()
                    .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
                    .map(|url| {
                        vec![ServerConfig {
                            url,
                            description: std::env::var("VESPERA_SERVER_DESCRIPTION").ok(),
                        }]
                    })
            }),
            merge,
        })
    }
}

/// Parse merge values: merge = [path::to::App, another::App]
fn parse_merge_values(input: ParseStream) -> syn::Result<Vec<syn::Path>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let paths: Punctuated<syn::Path, syn::Token![,]> =
        content.parse_terminated(syn::Path::parse, syn::Token![,])?;
    Ok(paths.into_iter().collect())
}

fn parse_openapi_values(input: ParseStream) -> syn::Result<Vec<LitStr>> {
    input.parse::<syn::Token![=]>()?;

    if input.peek(syn::token::Bracket) {
        let content;
        let _ = bracketed!(content in input);
        let entries: Punctuated<LitStr, syn::Token![,]> =
            content.parse_terminated(|input| input.parse::<LitStr>(), syn::Token![,])?;
        Ok(entries.into_iter().collect())
    } else {
        let single: LitStr = input.parse()?;
        Ok(vec![single])
    }
}

/// Validate that a URL starts with http:// or https://
fn validate_server_url(url: &LitStr) -> syn::Result<String> {
    let url_value = url.value();
    if !url_value.starts_with("http://") && !url_value.starts_with("https://") {
        return Err(syn::Error::new(
            url.span(),
            format!(
                "invalid server URL: `{}`. URL must start with `http://` or `https://`",
                url_value
            ),
        ));
    }
    Ok(url_value)
}

/// Parse server values in various formats:
/// - `servers = "url"` - single URL
/// - `servers = ["url1", "url2"]` - multiple URLs (strings only)
/// - `servers = [("url", "description")]` - tuple format with descriptions
/// - `servers = [{url = "...", description = "..."}]` - struct-like format
/// - `servers = {url = "...", description = "..."}` - single server struct-like format
fn parse_servers_values(input: ParseStream) -> syn::Result<Vec<ServerConfig>> {
    use syn::token::{Brace, Paren};

    input.parse::<syn::Token![=]>()?;

    if input.peek(syn::token::Bracket) {
        // Array format: [...]
        let content;
        let _ = bracketed!(content in input);

        let mut servers = Vec::new();

        while !content.is_empty() {
            if content.peek(Paren) {
                // Parse tuple: ("url", "description")
                let tuple_content;
                syn::parenthesized!(tuple_content in content);
                let url: LitStr = tuple_content.parse()?;
                let url_value = validate_server_url(&url)?;
                let description = if tuple_content.peek(syn::Token![,]) {
                    tuple_content.parse::<syn::Token![,]>()?;
                    Some(tuple_content.parse::<LitStr>()?.value())
                } else {
                    None
                };
                servers.push(ServerConfig {
                    url: url_value,
                    description,
                });
            } else if content.peek(Brace) {
                // Parse struct-like: {url = "...", description = "..."}
                let server = parse_server_struct(&content)?;
                servers.push(server);
            } else {
                // Parse simple string: "url"
                let url: LitStr = content.parse()?;
                let url_value = validate_server_url(&url)?;
                servers.push(ServerConfig {
                    url: url_value,
                    description: None,
                });
            }

            if content.peek(syn::Token![,]) {
                content.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(servers)
    } else if input.peek(syn::token::Brace) {
        // Single struct-like format: servers = {url = "...", description = "..."}
        let server = parse_server_struct(input)?;
        Ok(vec![server])
    } else {
        // Single string: servers = "url"
        let single: LitStr = input.parse()?;
        let url_value = validate_server_url(&single)?;
        Ok(vec![ServerConfig {
            url: url_value,
            description: None,
        }])
    }
}

/// Parse a single server in struct-like format: {url = "...", description = "..."}
fn parse_server_struct(input: ParseStream) -> syn::Result<ServerConfig> {
    let content;
    syn::braced!(content in input);

    let mut url: Option<String> = None;
    let mut description: Option<String> = None;

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();

        match ident_str.as_str() {
            "url" => {
                content.parse::<syn::Token![=]>()?;
                let url_lit: LitStr = content.parse()?;
                url = Some(validate_server_url(&url_lit)?);
            }
            "description" => {
                content.parse::<syn::Token![=]>()?;
                description = Some(content.parse::<LitStr>()?.value());
            }
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!(
                        "unknown field: `{}`. Expected `url` or `description`",
                        ident_str
                    ),
                ));
            }
        }

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    let url = url.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "server config requires `url` field",
        )
    })?;

    Ok(ServerConfig { url, description })
}

/// Docs info tuple type alias for cleaner signatures
type DocsInfo = (Option<(String, String)>, Option<(String, String)>);

/// Processed vespera input with extracted values
struct ProcessedVesperaInput {
    folder_name: String,
    openapi_file_names: Vec<String>,
    title: Option<String>,
    version: Option<String>,
    docs_url: Option<String>,
    redoc_url: Option<String>,
    servers: Option<Vec<Server>>,
    /// Apps to merge (syn::Path for code generation)
    merge: Vec<syn::Path>,
}

/// Process AutoRouterInput into extracted values
fn process_vespera_input(input: AutoRouterInput) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: input
            .dir
            .map(|f| f.value())
            .unwrap_or_else(|| "routes".to_string()),
        openapi_file_names: input
            .openapi
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.value())
            .collect(),
        title: input.title.map(|t| t.value()),
        version: input.version.map(|v| v.value()),
        docs_url: input.docs_url.map(|u| u.value()),
        redoc_url: input.redoc_url.map(|u| u.value()),
        servers: input.servers.map(|svrs| {
            svrs.into_iter()
                .map(|s| Server {
                    url: s.url,
                    description: s.description,
                    variables: None,
                })
                .collect()
        }),
        merge: input.merge.unwrap_or_default(),
    }
}

/// Generate OpenAPI JSON and write to files, returning docs info
fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
) -> Result<DocsInfo, String> {
    if input.openapi_file_names.is_empty() && input.docs_url.is_none() && input.redoc_url.is_none()
    {
        return Ok((None, None));
    }

    let mut openapi_doc = generate_openapi_doc_with_metadata(
        input.title.clone(),
        input.version.clone(),
        input.servers.clone(),
        metadata,
    );

    // Merge specs from child apps at compile time
    if !input.merge.is_empty()
        && let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR")
    {
        let manifest_path = Path::new(&manifest_dir);
        let target_dir = find_target_dir(manifest_path);
        let vespera_dir = target_dir.join("vespera");

        for merge_path in &input.merge {
            // Extract the struct name (last segment, e.g., "ThirdApp" from "third::ThirdApp")
            if let Some(last_segment) = merge_path.segments.last() {
                let struct_name = last_segment.ident.to_string();
                let spec_file = vespera_dir.join(format!("{}.openapi.json", struct_name));

                if let Ok(spec_content) = std::fs::read_to_string(&spec_file)
                    && let Ok(child_spec) =
                        serde_json::from_str::<vespera_core::openapi::OpenApi>(&spec_content)
                {
                    openapi_doc.merge(child_spec);
                }
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&openapi_doc)
        .map_err(|e| format!("Failed to serialize OpenAPI document: {}", e))?;

    for openapi_file_name in &input.openapi_file_names {
        let file_path = Path::new(openapi_file_name);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        std::fs::write(file_path, &json_str).map_err(|e| {
            format!(
                "Failed to write OpenAPI document to {}: {}",
                openapi_file_name, e
            )
        })?;
    }

    let docs_info = input
        .docs_url
        .as_ref()
        .map(|url| (url.clone(), json_str.clone()));
    let redoc_info = input.redoc_url.as_ref().map(|url| (url.clone(), json_str));

    Ok((docs_info, redoc_info))
}

#[proc_macro]
pub fn vespera(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as AutoRouterInput);
    let processed = process_vespera_input(input);

    let folder_path = find_folder_path(&processed.folder_name);
    if !folder_path.exists() {
        return syn::Error::new(
            Span::call_site(),
            format!("Folder not found: {}", processed.folder_name),
        )
        .to_compile_error()
        .into();
    }

    let mut metadata = match collect_metadata(&folder_path, &processed.folder_name) {
        Ok(m) => m,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to collect metadata: {}", e),
            )
            .to_compile_error()
            .into();
        }
    };
    metadata
        .structs
        .extend(SCHEMA_STORAGE.lock().unwrap().clone());

    let (docs_info, redoc_info) = match generate_and_write_openapi(&processed, &metadata) {
        Ok(info) => info,
        Err(e) => {
            return syn::Error::new(Span::call_site(), e)
                .to_compile_error()
                .into();
        }
    };

    generate_router_code(&metadata, docs_info, redoc_info, &processed.merge).into()
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

/// Find the workspace root's target directory
fn find_target_dir(manifest_path: &Path) -> std::path::PathBuf {
    // Look for workspace root by finding a Cargo.toml with [workspace] section
    let mut current = Some(manifest_path);
    let mut last_with_lock = None;

    while let Some(dir) = current {
        // Check if this directory has Cargo.lock
        if dir.join("Cargo.lock").exists() {
            last_with_lock = Some(dir.to_path_buf());
        }

        // Check if this is a workspace root (has Cargo.toml with [workspace])
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists()
            && let Ok(contents) = std::fs::read_to_string(&cargo_toml)
            && contents.contains("[workspace]")
        {
            return dir.join("target");
        }

        current = dir.parent();
    }

    // If we found a Cargo.lock but no [workspace], use the topmost one
    if let Some(lock_dir) = last_with_lock {
        return lock_dir.join("target");
    }

    // Fallback: use manifest dir's target
    manifest_path.join("target")
}

fn generate_router_code(
    metadata: &CollectedMetadata,
    docs_info: Option<(String, String)>,
    redoc_info: Option<(String, String)>,
    merge_apps: &[syn::Path],
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
        let func_name = syn::Ident::new(function_name, Span::call_site());
        router_nests.push(quote!(
            .route(#path, #method_path(#p::#func_name))
        ));
    }

    // Check if we need to merge specs at runtime
    let has_merge = !merge_apps.is_empty();

    if let Some((docs_url, spec)) = docs_info {
        let method_path = http_method_to_token_stream(HttpMethod::Get);

        if has_merge {
            // Generate code that merges specs at runtime using OnceLock
            let merge_spec_code: Vec<_> = merge_apps
                .iter()
                .map(|app_path| {
                    quote! {
                        if let Ok(other) = vespera::serde_json::from_str::<vespera::OpenApi>(#app_path::OPENAPI_SPEC) {
                            merged.merge(other);
                        }
                    }
                })
                .collect();

            router_nests.push(quote!(
                .route(#docs_url, #method_path(|| async {
                    static MERGED_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                    let spec = MERGED_SPEC.get_or_init(|| {
                        let base_spec = #spec;
                        let mut merged: vespera::OpenApi = vespera::serde_json::from_str(base_spec).unwrap();
                        #(#merge_spec_code)*
                        vespera::serde_json::to_string(&merged).unwrap()
                    });
                    let html = format!(
                        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Swagger UI</title><link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist/swagger-ui.css" /></head><body style="margin: 0; padding: 0;"><div id="swagger-ui"></div><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js"></script><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-standalone-preset.js"></script><script>const openapiSpec = {};window.onload = () => {{ SwaggerUIBundle({{ spec: openapiSpec, dom_id: "\#swagger-ui", presets: [SwaggerUIBundle.presets.apis, SwaggerUIStandalonePreset], layout: "StandaloneLayout" }}); }};</script></body></html>"#,
                        spec
                    );
                    vespera::axum::response::Html(html)
                }))
            ));
        } else {
            let html = format!(
                r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Swagger UI</title><link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist/swagger-ui.css" /></head><body style="margin: 0; padding: 0;"><div id="swagger-ui"></div><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js"></script><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-standalone-preset.js"></script><script>const openapiSpec = {spec_json};window.onload = () => {{ SwaggerUIBundle({{ spec: openapiSpec, dom_id: "\#swagger-ui", presets: [SwaggerUIBundle.presets.apis, SwaggerUIStandalonePreset], layout: "StandaloneLayout" }}); }};</script></body></html>"#,
                spec_json = spec
            );

            router_nests.push(quote!(
                .route(#docs_url, #method_path(|| async { vespera::axum::response::Html(#html) }))
            ));
        }
    }

    if let Some((redoc_url, spec)) = redoc_info {
        let method_path = http_method_to_token_stream(HttpMethod::Get);

        if has_merge {
            // Generate code that merges specs at runtime using OnceLock
            let merge_spec_code: Vec<_> = merge_apps
                .iter()
                .map(|app_path| {
                    quote! {
                        if let Ok(other) = vespera::serde_json::from_str::<vespera::OpenApi>(#app_path::OPENAPI_SPEC) {
                            merged.merge(other);
                        }
                    }
                })
                .collect();

            router_nests.push(quote!(
                .route(#redoc_url, #method_path(|| async {
                    static MERGED_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                    let spec = MERGED_SPEC.get_or_init(|| {
                        let base_spec = #spec;
                        let mut merged: vespera::OpenApi = vespera::serde_json::from_str(base_spec).unwrap();
                        #(#merge_spec_code)*
                        vespera::serde_json::to_string(&merged).unwrap()
                    });
                    let html = format!(
                        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body {{ margin: 0; padding: 0; }}</style><link rel="stylesheet" href="https://unpkg.com/redoc/bundles/redoc.standalone.css" /></head><body><div id="redoc-container"></div><script src="https://unpkg.com/redoc/bundles/redoc.standalone.js"></script><script>const openapiSpec = {};Redoc.init(openapiSpec, {{}}, document.getElementById("redoc-container"));</script></body></html>"#,
                        spec
                    );
                    vespera::axum::response::Html(html)
                }))
            ));
        } else {
            let html = format!(
                r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body {{ margin: 0; padding: 0; }}</style><link rel="stylesheet" href="https://unpkg.com/redoc/bundles/redoc.standalone.css" /></head><body><div id="redoc-container"></div><script src="https://unpkg.com/redoc/bundles/redoc.standalone.js"></script><script>const openapiSpec = {spec_json};Redoc.init(openapiSpec, {{}}, document.getElementById("redoc-container"));</script></body></html>"#,
                spec_json = spec
            );

            router_nests.push(quote!(
                .route(#redoc_url, #method_path(|| async { vespera::axum::response::Html(#html) }))
            ));
        }
    }

    if merge_apps.is_empty() {
        quote! {
            vespera::axum::Router::new()
                #( #router_nests )*
        }
    } else {
        // When merging apps, return VesperaRouter which defers the merge
        // until with_state() is called. This is necessary because Axum requires
        // merged routers to have the same state type.
        quote! {
            vespera::VesperaRouter::new(
                vespera::axum::Router::new()
                    #( #router_nests )*,
                vec![#( #merge_apps::router ),*]
            )
        }
    }
}

/// Input for export_app! macro
struct ExportAppInput {
    /// App name (struct name to generate)
    name: syn::Ident,
    /// Route directory
    dir: Option<LitStr>,
}

impl Parse for ExportAppInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;

        let mut dir = None;

        // Parse optional comma and arguments
        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: syn::Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "dir" => {
                    input.parse::<syn::Token![=]>()?;
                    dir = Some(input.parse()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown field: `{}`. Expected `dir`", ident_str),
                    ));
                }
            }
        }

        Ok(ExportAppInput { name, dir })
    }
}

/// Export a vespera app as a reusable component.
///
/// Generates a struct with:
/// - `OPENAPI_SPEC: &'static str` - The OpenAPI JSON spec
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
#[proc_macro]
pub fn export_app(input: TokenStream) -> TokenStream {
    let ExportAppInput { name, dir } = syn::parse_macro_input!(input as ExportAppInput);

    let folder_name = dir
        .map(|d| d.value())
        .or_else(|| std::env::var("VESPERA_DIR").ok())
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

    let mut metadata = match collect_metadata(&folder_path, &folder_name) {
        Ok(m) => m,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to collect metadata: {}", e),
            )
            .to_compile_error()
            .into();
        }
    };
    metadata
        .structs
        .extend(SCHEMA_STORAGE.lock().unwrap().clone());

    // Generate OpenAPI spec JSON string
    let openapi_doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);
    let spec_json = match serde_json::to_string(&openapi_doc) {
        Ok(json) => json,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("Failed to serialize OpenAPI spec: {}", e),
            )
            .to_compile_error()
            .into();
        }
    };

    // Write spec to temp file for compile-time merging by parent apps
    // The file is written to target/vespera/{StructName}.openapi.json
    let name_str = name.to_string();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    // Find target directory (go up from manifest dir to workspace root if needed)
    let manifest_path = Path::new(&manifest_dir);
    let target_dir = find_target_dir(manifest_path);
    let vespera_dir = target_dir.join("vespera");
    std::fs::create_dir_all(&vespera_dir)
        .unwrap_or_else(|e| panic!("Failed to create vespera dir {:?}: {}", vespera_dir, e));
    let spec_file = vespera_dir.join(format!("{}.openapi.json", name_str));
    std::fs::write(&spec_file, &spec_json)
        .unwrap_or_else(|e| panic!("Failed to write spec file {:?}: {}", spec_file, e));

    // Generate router code (without docs routes, no merge)
    let router_code = generate_router_code(&metadata, None, None, &[]);

    quote! {
        /// Auto-generated vespera app struct
        pub struct #name;

        impl #name {
            /// OpenAPI specification as JSON string
            pub const OPENAPI_SPEC: &'static str = #spec_json;

            /// Create the router for this app.
            /// Returns `Router<()>` which can be merged into any other router.
            pub fn router() -> vespera::axum::Router<()> {
                #router_code
            }
        }
    }
    .into()
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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
        "/create-user",
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
        "/update-user",
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
        "/delete-user",
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
        "/patch-user",
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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
            &collect_metadata(temp_dir.path(), folder_name).unwrap(),
            None,
            None,
            &[],
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

    // ========== Tests for parsing functions ==========

    #[test]
    fn test_parse_openapi_values_single() {
        // Test that single string openapi value parses correctly via AutoRouterInput
        let tokens = quote::quote!(openapi = "openapi.json");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let openapi = input.openapi.unwrap();
        assert_eq!(openapi.len(), 1);
        assert_eq!(openapi[0].value(), "openapi.json");
    }

    #[test]
    fn test_parse_openapi_values_array() {
        // Test that array openapi value parses correctly via AutoRouterInput
        let tokens = quote::quote!(openapi = ["openapi.json", "api.json"]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let openapi = input.openapi.unwrap();
        assert_eq!(openapi.len(), 2);
        assert_eq!(openapi[0].value(), "openapi.json");
        assert_eq!(openapi[1].value(), "api.json");
    }

    #[test]
    fn test_validate_server_url_valid_http() {
        let lit = LitStr::new("http://localhost:3000", Span::call_site());
        let result = validate_server_url(&lit);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "http://localhost:3000");
    }

    #[test]
    fn test_validate_server_url_valid_https() {
        let lit = LitStr::new("https://api.example.com", Span::call_site());
        let result = validate_server_url(&lit);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://api.example.com");
    }

    #[test]
    fn test_validate_server_url_invalid() {
        let lit = LitStr::new("ftp://example.com", Span::call_site());
        let result = validate_server_url(&lit);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_server_url_no_scheme() {
        let lit = LitStr::new("example.com", Span::call_site());
        let result = validate_server_url(&lit);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_router_input_parse_dir_only() {
        let tokens = quote::quote!(dir = "api");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.dir.unwrap().value(), "api");
        assert!(input.openapi.is_none());
    }

    #[test]
    fn test_auto_router_input_parse_string_as_dir() {
        let tokens = quote::quote!("routes");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.dir.unwrap().value(), "routes");
    }

    #[test]
    fn test_auto_router_input_parse_openapi_single() {
        let tokens = quote::quote!(openapi = "openapi.json");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let openapi = input.openapi.unwrap();
        assert_eq!(openapi.len(), 1);
        assert_eq!(openapi[0].value(), "openapi.json");
    }

    #[test]
    fn test_auto_router_input_parse_openapi_array() {
        let tokens = quote::quote!(openapi = ["a.json", "b.json"]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let openapi = input.openapi.unwrap();
        assert_eq!(openapi.len(), 2);
    }

    #[test]
    fn test_auto_router_input_parse_title_version() {
        let tokens = quote::quote!(title = "My API", version = "2.0.0");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.title.unwrap().value(), "My API");
        assert_eq!(input.version.unwrap().value(), "2.0.0");
    }

    #[test]
    fn test_auto_router_input_parse_docs_redoc() {
        let tokens = quote::quote!(docs_url = "/docs", redoc_url = "/redoc");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.docs_url.unwrap().value(), "/docs");
        assert_eq!(input.redoc_url.unwrap().value(), "/redoc");
    }

    #[test]
    fn test_auto_router_input_parse_servers_single() {
        let tokens = quote::quote!(servers = "http://localhost:3000");
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
        assert!(servers[0].description.is_none());
    }

    #[test]
    fn test_auto_router_input_parse_servers_array_strings() {
        let tokens = quote::quote!(servers = ["http://localhost:3000", "https://api.example.com"]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 2);
    }

    #[test]
    fn test_auto_router_input_parse_servers_tuple() {
        let tokens = quote::quote!(servers = [("http://localhost:3000", "Development")]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
        assert_eq!(servers[0].description, Some("Development".to_string()));
    }

    #[test]
    fn test_auto_router_input_parse_servers_struct() {
        let tokens =
            quote::quote!(servers = [{ url = "http://localhost:3000", description = "Dev" }]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
        assert_eq!(servers[0].description, Some("Dev".to_string()));
    }

    #[test]
    fn test_auto_router_input_parse_servers_single_struct() {
        let tokens = quote::quote!(servers = { url = "https://api.example.com" });
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "https://api.example.com");
    }

    #[test]
    fn test_auto_router_input_parse_unknown_field() {
        let tokens = quote::quote!(unknown_field = "value");
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_router_input_parse_all_fields() {
        let tokens = quote::quote!(
            dir = "api",
            openapi = "openapi.json",
            title = "Test API",
            version = "1.0.0",
            docs_url = "/docs",
            redoc_url = "/redoc",
            servers = "http://localhost:3000"
        );
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        assert!(input.dir.is_some());
        assert!(input.openapi.is_some());
        assert!(input.title.is_some());
        assert!(input.version.is_some());
        assert!(input.docs_url.is_some());
        assert!(input.redoc_url.is_some());
        assert!(input.servers.is_some());
    }

    #[test]
    fn test_generate_router_code_with_docs() {
        let metadata = CollectedMetadata::new();
        let docs_info = Some(("/docs".to_string(), r#"{"openapi":"3.1.0"}"#.to_string()));

        let result = generate_router_code(&metadata, docs_info, None, &[]);
        let code = result.to_string();

        assert!(code.contains("/docs"));
        assert!(code.contains("swagger-ui"));
    }

    #[test]
    fn test_generate_router_code_with_redoc() {
        let metadata = CollectedMetadata::new();
        let redoc_info = Some(("/redoc".to_string(), r#"{"openapi":"3.1.0"}"#.to_string()));

        let result = generate_router_code(&metadata, None, redoc_info, &[]);
        let code = result.to_string();

        assert!(code.contains("/redoc"));
        assert!(code.contains("redoc"));
    }

    #[test]
    fn test_generate_router_code_with_both_docs() {
        let metadata = CollectedMetadata::new();
        let docs_info = Some(("/docs".to_string(), r#"{"openapi":"3.1.0"}"#.to_string()));
        let redoc_info = Some(("/redoc".to_string(), r#"{"openapi":"3.1.0"}"#.to_string()));

        let result = generate_router_code(&metadata, docs_info, redoc_info, &[]);
        let code = result.to_string();

        assert!(code.contains("/docs"));
        assert!(code.contains("/redoc"));
    }

    #[test]
    fn test_parse_server_struct_url_only() {
        // Test server struct parsing via AutoRouterInput
        let tokens = quote::quote!(servers = { url = "http://localhost:3000" });
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
        assert!(servers[0].description.is_none());
    }

    #[test]
    fn test_parse_server_struct_with_description() {
        let tokens =
            quote::quote!(servers = { url = "http://localhost:3000", description = "Local" });
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers[0].description, Some("Local".to_string()));
    }

    #[test]
    fn test_parse_server_struct_unknown_field() {
        let tokens = quote::quote!(servers = { url = "http://localhost:3000", unknown = "test" });
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_server_struct_missing_url() {
        let tokens = quote::quote!(servers = { description = "test" });
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_servers_tuple_url_only() {
        let tokens = quote::quote!(servers = [("http://localhost:3000")]);
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let servers = input.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert!(servers[0].description.is_none());
    }

    #[test]
    fn test_parse_servers_invalid_url() {
        let tokens = quote::quote!(servers = "invalid-url");
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_router_input_parse_invalid_token() {
        // Test line 149: neither ident nor string literal triggers lookahead error
        let tokens = quote::quote!(123);
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_router_input_empty() {
        // Test empty input - should use defaults/env vars
        let tokens = quote::quote!();
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_router_input_multiple_commas() {
        // Test input with trailing comma
        let tokens = quote::quote!(dir = "api",);
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_router_input_no_comma() {
        // Test input without comma between fields (should stop at second field)
        let tokens = quote::quote!(dir = "api" title = "Test");
        let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
        // This should fail or only parse first field
        assert!(result.is_err());
    }

    // ========== Tests for validate_route_fn ==========

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

    // ========== Tests for process_derive_schema ==========

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

    // ========== Tests for process_vespera_input ==========

    #[test]
    fn test_process_vespera_input_defaults() {
        let tokens = quote::quote!();
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        assert_eq!(processed.folder_name, "routes");
        assert!(processed.openapi_file_names.is_empty());
        assert!(processed.title.is_none());
        assert!(processed.docs_url.is_none());
    }

    #[test]
    fn test_process_vespera_input_all_fields() {
        let tokens = quote::quote!(
            dir = "api",
            openapi = ["openapi.json", "api.json"],
            title = "My API",
            version = "1.0.0",
            docs_url = "/docs",
            redoc_url = "/redoc",
            servers = "http://localhost:3000"
        );
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        assert_eq!(processed.folder_name, "api");
        assert_eq!(
            processed.openapi_file_names,
            vec!["openapi.json", "api.json"]
        );
        assert_eq!(processed.title, Some("My API".to_string()));
        assert_eq!(processed.version, Some("1.0.0".to_string()));
        assert_eq!(processed.docs_url, Some("/docs".to_string()));
        assert_eq!(processed.redoc_url, Some("/redoc".to_string()));
        let servers = processed.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
    }

    #[test]
    fn test_process_vespera_input_servers_with_description() {
        let tokens = quote::quote!(
            servers = [{ url = "https://api.example.com", description = "Production" }]
        );
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        let servers = processed.servers.unwrap();
        assert_eq!(servers[0].url, "https://api.example.com");
        assert_eq!(servers[0].description, Some("Production".to_string()));
    }

    // ========== Tests for generate_and_write_openapi ==========

    #[test]
    fn test_generate_and_write_openapi_no_output() {
        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_none());
        assert!(redoc_info.is_none());
    }

    #[test]
    fn test_generate_and_write_openapi_docs_only() {
        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: Some("Test API".to_string()),
            version: Some("1.0.0".to_string()),
            docs_url: Some("/docs".to_string()),
            redoc_url: None,
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_some());
        let (url, json) = docs_info.unwrap();
        assert_eq!(url, "/docs");
        assert!(json.contains("\"openapi\""));
        assert!(json.contains("Test API"));
        assert!(redoc_info.is_none());
    }

    #[test]
    fn test_generate_and_write_openapi_redoc_only() {
        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: Some("/redoc".to_string()),
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_none());
        assert!(redoc_info.is_some());
        let (url, _) = redoc_info.unwrap();
        assert_eq!(url, "/redoc");
    }

    #[test]
    fn test_generate_and_write_openapi_both_docs() {
        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: Some("/docs".to_string()),
            redoc_url: Some("/redoc".to_string()),
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());
        let (docs_info, redoc_info) = result.unwrap();
        assert!(docs_info.is_some());
        assert!(redoc_info.is_some());
    }

    #[test]
    fn test_generate_and_write_openapi_file_output() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let output_path = temp_dir.path().join("test-openapi.json");

        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![output_path.to_string_lossy().to_string()],
            title: Some("File Test".to_string()),
            version: Some("2.0.0".to_string()),
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());

        // Verify file was written
        assert!(output_path.exists());
        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("\"openapi\""));
        assert!(content.contains("File Test"));
        assert!(content.contains("2.0.0"));
    }

    #[test]
    fn test_generate_and_write_openapi_creates_directories() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let output_path = temp_dir.path().join("nested/dir/openapi.json");

        let processed = ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![output_path.to_string_lossy().to_string()],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            merge: vec![],
        };
        let metadata = CollectedMetadata::new();
        let result = generate_and_write_openapi(&processed, &metadata);
        assert!(result.is_ok());

        // Verify nested directories and file were created
        assert!(output_path.exists());
    }

    // ========== Tests for find_folder_path ==========
    // Note: find_folder_path uses CARGO_MANIFEST_DIR which is set during cargo test

    #[test]
    fn test_find_folder_path_nonexistent_returns_path() {
        // When the constructed path doesn't exist, it falls back to using folder_name directly
        let result = find_folder_path("nonexistent_folder_xyz");
        // It should return a PathBuf (either from src/nonexistent... or just the folder name)
        assert!(result.to_string_lossy().contains("nonexistent_folder_xyz"));
    }
}
