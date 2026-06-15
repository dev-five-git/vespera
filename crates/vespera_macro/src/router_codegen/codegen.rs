//! Router `TokenStream` generation.
//!
//! Owns the Swagger / ReDoc HTML templates, the cron-scheduler spawn code,
//! and [`generate_router_code`] — the function that stitches collected route
//! metadata into an `axum::Router` literal.

use proc_macro2::Span;
use quote::quote;
use vespera_core::route::HttpMethod;

use crate::{
    metadata::{CollectedMetadata, CronMetadata},
    method::http_method_to_token_stream,
};

/// Swagger UI HTML template. Contains `{}` format placeholder for the OpenAPI spec JSON.
const SWAGGER_UI_HTML: &str = r##"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Swagger UI</title><link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist/swagger-ui.css" /></head><body style="margin: 0; padding: 0;"><div id="swagger-ui"></div><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js"></script><script src="https://unpkg.com/swagger-ui-dist/swagger-ui-standalone-preset.js"></script><script>const openapiSpec = {};window.onload = () => {{ SwaggerUIBundle({{ spec: openapiSpec, dom_id: "#swagger-ui", presets: [SwaggerUIBundle.presets.apis, SwaggerUIStandalonePreset], layout: "StandaloneLayout" }}); }};</script></body></html>"##;

/// ReDoc HTML template. Contains `{}` format placeholder for the OpenAPI spec JSON.
const REDOC_HTML: &str = r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body {{ margin: 0; padding: 0; }}</style><link rel="stylesheet" href="https://unpkg.com/redoc/bundles/redoc.standalone.css" /></head><body><div id="redoc-container"></div><script src="https://unpkg.com/redoc/bundles/redoc.standalone.js"></script><script>const openapiSpec = {};Redoc.init(openapiSpec, {{}}, document.getElementById("redoc-container"));</script></body></html>"#;

/// Generate a documentation route handler (Swagger UI or ReDoc).
///
/// When `has_merge` is true, the handler merges specs from child apps at runtime.
/// When false, it serves the spec directly from the compile-time constant.
fn generate_docs_route_tokens(
    url: &str,
    html_template: &str,
    merge_spec_code: &[proc_macro2::TokenStream],
    has_merge: bool,
) -> proc_macro2::TokenStream {
    let method_path = http_method_to_token_stream(HttpMethod::Get);

    if has_merge {
        quote!(
            .route(#url, #method_path(|| async {
                static MERGED_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let spec = MERGED_SPEC.get_or_init(|| {
                    let mut merged: vespera::OpenApi = vespera::serde_json::from_str(__VESPERA_SPEC).unwrap();
                    #(#merge_spec_code)*
                    vespera::serde_json::to_string(&merged).unwrap()
                });
                static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let html = HTML.get_or_init(|| {
                    format!(#html_template, spec)
                });
                vespera::axum::response::Html(html.as_str())
            }))
        )
    } else {
        quote!(
            .route(#url, #method_path(|| async {
                static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let html = HTML.get_or_init(|| {
                    format!(#html_template, __VESPERA_SPEC)
                });
                vespera::axum::response::Html(html.as_str())
            }))
        )
    }
}

/// Generate cron scheduler spawn code from collected cron metadata.
fn generate_cron_scheduler_code(cron_jobs: &[CronMetadata]) -> proc_macro2::TokenStream {
    if cron_jobs.is_empty() {
        return quote!();
    }

    let job_additions: Vec<proc_macro2::TokenStream> = cron_jobs
        .iter()
        .map(|cron| {
            let expression = &cron.expression;
            let module_path = &cron.module_path;
            let function_name = &cron.function_name;

            // Build the full path: crate::module::function
            let mut p: syn::punctuated::Punctuated<syn::PathSegment, syn::Token![::]> =
                syn::punctuated::Punctuated::new();
            p.push(syn::PathSegment {
                ident: syn::Ident::new("crate", Span::call_site()),
                arguments: syn::PathArguments::None,
            });
            p.extend(module_path.split("::").filter_map(|s| {
                if s.is_empty() {
                    None
                } else {
                    Some(syn::PathSegment {
                        ident: syn::Ident::new(s, Span::call_site()),
                        arguments: syn::PathArguments::None,
                    })
                }
            }));
            let func_ident = syn::Ident::new(function_name, Span::call_site());

            let err_create = format!("vespera: failed to create cron job '{function_name}'");
            let err_add = format!("vespera: failed to add cron job '{function_name}'");

            quote! {
                __vespera_cron_scheduler.add(
                    vespera::tokio_cron_scheduler::Job::new_async(#expression, |_uuid, _l| {
                        Box::pin(async move {
                            #p::#func_ident().await;
                        })
                    }).expect(#err_create)
                ).await.expect(#err_add);
            }
        })
        .collect();

    quote! {
        vespera::tokio::spawn(async move {
            let mut __vespera_cron_scheduler = vespera::tokio_cron_scheduler::JobScheduler::new().await
                .expect("vespera: failed to create cron scheduler");
            #(#job_additions)*
            __vespera_cron_scheduler.start().await
                .expect("vespera: failed to start cron scheduler");
            // Keep scheduler alive forever
            ::std::future::pending::<()>().await;
        });
    }
}

/// Generate Axum router code from collected metadata
#[allow(clippy::too_many_lines)]
pub fn generate_router_code(
    metadata: &CollectedMetadata,
    docs_url: Option<&str>,
    redoc_url: Option<&str>,
    spec_tokens: Option<proc_macro2::TokenStream>,
    merge_apps: &[syn::Path],
    cron_jobs: &[CronMetadata],
) -> proc_macro2::TokenStream {
    let mut router_nests = Vec::new();

    for route in &metadata.routes {
        let Ok(http_method) = HttpMethod::try_from(route.method.as_str()) else {
            eprintln!(
                "vespera: skipping route '{}' — unknown HTTP method '{}'",
                route.path, route.method
            );
            continue;
        };
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
        p.extend(module_path.split("::").filter_map(|s| {
            if s.is_empty() {
                None
            } else {
                Some(syn::PathSegment {
                    ident: syn::Ident::new(s, Span::call_site()),
                    arguments: syn::PathArguments::None,
                })
            }
        }));
        let func_name = syn::Ident::new(function_name, Span::call_site());
        router_nests.push(quote!(
            .route(#path, #method_path(#p::#func_name))
        ));
    }

    // Check if we need to merge specs at runtime
    let has_merge = !merge_apps.is_empty();

    // Generate merge code once, reuse in both docs_url and redoc_url routes
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

    if let Some(docs_url) = docs_url {
        router_nests.push(generate_docs_route_tokens(
            docs_url,
            SWAGGER_UI_HTML,
            &merge_spec_code,
            has_merge,
        ));
    }

    if let Some(redoc_url) = redoc_url {
        router_nests.push(generate_docs_route_tokens(
            redoc_url,
            REDOC_HTML,
            &merge_spec_code,
            has_merge,
        ));
    }

    let needs_spec_const = spec_tokens.is_some() && (docs_url.is_some() || redoc_url.is_some());
    let cron_code = generate_cron_scheduler_code(cron_jobs);

    if needs_spec_const {
        let spec_expr = spec_tokens.unwrap();
        if merge_apps.is_empty() {
            quote! {
                {
                    const __VESPERA_SPEC: &str = #spec_expr;
                    #cron_code
                    vespera::axum::Router::new()
                        #( #router_nests )*
                }
            }
        } else {
            quote! {
                {
                    const __VESPERA_SPEC: &str = #spec_expr;
                    #cron_code
                    vespera::VesperaRouter::new(
                        vespera::axum::Router::new()
                            #( #router_nests )*,
                        vec![#( #merge_apps::router ),*]
                    )
                }
            }
        }
    } else if merge_apps.is_empty() {
        if cron_jobs.is_empty() {
            quote! {
                vespera::axum::Router::new()
                    #( #router_nests )*
            }
        } else {
            quote! {
                {
                    #cron_code
                    vespera::axum::Router::new()
                        #( #router_nests )*
                }
            }
        }
    } else {
        // When merging apps, return VesperaRouter which defers the merge
        // until with_state() is called. This is necessary because Axum requires
        // merged routers to have the same state type.
        if cron_jobs.is_empty() {
            quote! {
                vespera::VesperaRouter::new(
                    vespera::axum::Router::new()
                        #( #router_nests )*,
                    vec![#( #merge_apps::router ),*]
                )
            }
        } else {
            quote! {
                {
                    #cron_code
                    vespera::VesperaRouter::new(
                        vespera::axum::Router::new()
                            #( #router_nests )*,
                        vec![#( #merge_apps::router ),*]
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;
    use crate::collector::collect_metadata;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    // ===== Empty / basic routers =====

    #[test]
    fn test_generate_router_code_empty() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let result = generate_router_code(
            &collect_metadata(temp_dir.path(), folder_name, &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {code}"
        );
        assert!(
            !code.contains("route"),
            "Code should not contain route, got: {code}"
        );

        drop(temp_dir);
    }

    /// Render the standard single-route fixture file body.
    fn route_src(route_attr: &str, fn_name: &str) -> String {
        format!("\n#[route({route_attr})]\npub fn {fn_name}() -> String {{\n\"x\".to_string()\n}}\n")
    }

    #[rstest]
    #[case::single_get_route("users.rs", "get", "get_users", "get", "/users", "routes::users::get_users")]
    #[case::single_post_route("create_user.rs", "post", "create_user", "post", "/create-user", "routes::create_user::create_user")]
    #[case::single_put_route("update_user.rs", "put", "update_user", "put", "/update-user", "routes::update_user::update_user")]
    #[case::single_delete_route("delete_user.rs", "delete", "delete_user", "delete", "/delete-user", "routes::delete_user::delete_user")]
    #[case::single_patch_route("patch_user.rs", "patch", "patch_user", "patch", "/patch-user", "routes::patch_user::patch_user")]
    #[case::route_with_custom_path("users.rs", r#"get, path = "/api/users""#, "get_users", "get", "/users/api/users", "routes::users::get_users")]
    #[case::nested_module("api/users.rs", "get", "get_users", "get", "/api/users", "routes::api::users::get_users")]
    #[case::deeply_nested_module("api/v1/users.rs", "get", "get_users", "get", "/api/v1/users", "routes::api::v1::users::get_users")]
    fn test_generate_router_code_single_route(
        #[case] filename: &str,
        #[case] route_attr: &str,
        #[case] fn_name: &str,
        #[case] expected_method: &str,
        #[case] expected_path: &str,
        #[case] expected_function_path: &str,
    ) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        create_temp_file(&temp_dir, filename, &route_src(route_attr, fn_name));

        let result = generate_router_code(
            &collect_metadata(temp_dir.path(), "routes", &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {code}"
        );

        assert!(
            code.contains(expected_method),
            "Code should contain method: {expected_method}, got: {code}"
        );

        assert!(
            code.contains(expected_path),
            "Code should contain path: {expected_path}, got: {code}"
        );

        let function_parts: Vec<&str> = expected_function_path.split("::").collect();
        for part in &function_parts {
            if !part.is_empty() {
                assert!(
                    code.contains(part),
                    "Code should contain function part: {part}, got: {code}"
                );
            }
        }

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_multiple_routes() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

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
            &collect_metadata(temp_dir.path(), folder_name, &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("Router") && code.contains("new"));

        assert!(code.contains("get_users"));
        assert!(code.contains("create_user"));
        assert!(code.contains("update_user"));

        assert!(code.contains("get"));
        assert!(code.contains("post"));
        assert!(code.contains("put"));

        let route_count = code.matches(". route (").count();
        assert_eq!(
            route_count, 3,
            "Should have 3 route calls, got: {route_count}, code: {code}"
        );

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_same_path_different_methods() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

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
            &collect_metadata(temp_dir.path(), folder_name, &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("Router") && code.contains("new"));

        assert!(code.contains("get_users"));
        assert!(code.contains("create_users"));

        assert!(code.contains("get"));
        assert!(code.contains("post"));

        let route_count = code.matches(". route (").count();
        assert_eq!(
            route_count, 2,
            "Should have 2 routes, got: {route_count}, code: {code}"
        );

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_with_mod_rs() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

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
            &collect_metadata(temp_dir.path(), folder_name, &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("Router") && code.contains("new"));
        assert!(code.contains("index"));
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
            &collect_metadata(temp_dir.path(), folder_name, &[])
                .unwrap()
                .0,
            None,
            None,
            None,
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("Router") && code.contains("new"));
        assert!(code.contains("get_users"));
        assert!(!code.contains("::users::users"));

        drop(temp_dir);
    }

    // ===== Docs & redoc routes =====

    #[test]
    fn test_generate_router_code_with_docs() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;

        let result = generate_router_code(
            &metadata,
            Some("/docs"),
            None,
            Some(quote::quote!(#spec)),
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("/docs"));
        assert!(code.contains("swagger-ui"));
        assert!(code.contains("__VESPERA_SPEC"));
        assert!(code.contains("OnceLock"));
    }

    #[test]
    fn test_generate_router_code_with_redoc() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;

        let result = generate_router_code(
            &metadata,
            None,
            Some("/redoc"),
            Some(quote::quote!(#spec)),
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("/redoc"));
        assert!(code.contains("redoc"));
        assert!(code.contains("__VESPERA_SPEC"));
        assert!(code.contains("OnceLock"));
    }

    #[test]
    fn test_generate_router_code_with_both_docs() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;

        let result = generate_router_code(
            &metadata,
            Some("/docs"),
            Some("/redoc"),
            Some(quote::quote!(#spec)),
            &[],
            &[],
        );
        let code = result.to_string();

        assert!(code.contains("/docs"));
        assert!(code.contains("/redoc"));
        assert!(code.contains("__VESPERA_SPEC"));
    }

    #[test]
    fn test_swagger_html_template_renders_valid_quotes() {
        assert!(
            !SWAGGER_UI_HTML.contains(r#"\""#),
            "Swagger template should not contain literal backslash-quotes: {SWAGGER_UI_HTML}"
        );
        assert!(
            SWAGGER_UI_HTML.contains(r#"href="https://unpkg.com/swagger-ui-dist/swagger-ui.css""#)
        );
        assert!(
            SWAGGER_UI_HTML
                .contains(r#"src="https://unpkg.com/swagger-ui-dist/swagger-ui-bundle.js""#)
        );
        assert!(SWAGGER_UI_HTML.contains(r##"dom_id: "#swagger-ui""##));
    }

    #[test]
    fn test_redoc_html_template_renders_valid_quotes() {
        assert!(
            !REDOC_HTML.contains(r#"\""#),
            "ReDoc template should not contain literal backslash-quotes: {REDOC_HTML}"
        );
        assert!(
            REDOC_HTML.contains(r#"href="https://unpkg.com/redoc/bundles/redoc.standalone.css""#)
        );
        assert!(REDOC_HTML.contains(r#"src="https://unpkg.com/redoc/bundles/redoc.standalone.js""#));
        assert!(REDOC_HTML.contains(r#"document.getElementById("redoc-container")"#));
    }

    // ===== Unknown method / route skipping =====

    #[test]
    fn test_generate_router_code_unknown_http_method() {
        let mut metadata = CollectedMetadata {
            routes: Vec::new(),
            structs: Vec::new(),
            crons: Vec::new(),
        };
        metadata.routes.push(crate::metadata::RouteMetadata {
            method: "INVALID".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "dummy.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let result = generate_router_code(&metadata, None, None, None, &[], &[]);
        let code = result.to_string();

        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {code}"
        );
        assert!(
            !code.contains(". route ("),
            "Route with unknown HTTP method should be skipped, got: {code}"
        );
    }

    #[test]
    fn test_generate_router_code_unknown_method_skipped_valid_kept() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

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

        let (mut metadata, _file_asts) =
            collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();
        metadata.routes.push(crate::metadata::RouteMetadata {
            method: "CONNECT".to_string(),
            path: "/invalid".to_string(),
            function_name: "connect_handler".to_string(),
            module_path: "routes::invalid".to_string(),
            file_path: "dummy.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let result = generate_router_code(&metadata, None, None, None, &[], &[]);
        let code = result.to_string();

        assert!(
            code.contains("get_users"),
            "Valid route should be present, got: {code}"
        );
        assert!(
            !code.contains("connect_handler"),
            "Invalid method route should be skipped, got: {code}"
        );

        drop(temp_dir);
    }

    // ===== Merge apps =====

    #[test]
    fn test_generate_router_code_with_merge_apps() {
        let metadata = CollectedMetadata::new();
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(third::ThirdApp)];

        let result = generate_router_code(&metadata, None, None, None, &merge_apps, &[]);
        let code = result.to_string();

        assert!(
            code.contains("VesperaRouter"),
            "Should use VesperaRouter for merge, got: {code}"
        );
        assert!(
            code.contains("third :: ThirdApp") || code.contains("third::ThirdApp"),
            "Should reference merged app, got: {code}"
        );
    }

    #[test]
    fn test_generate_router_code_with_docs_and_merge() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(app::MyApp)];

        let result = generate_router_code(
            &metadata,
            Some("/docs"),
            None,
            Some(quote::quote!(#spec)),
            &merge_apps,
            &[],
        );
        let code = result.to_string();

        assert!(
            code.contains("OnceLock"),
            "Should use OnceLock for merged docs, got: {code}"
        );
        assert!(
            code.contains("MERGED_SPEC"),
            "Should have MERGED_SPEC, got: {code}"
        );
        assert!(
            code.contains("merged . merge") || code.contains("merged.merge"),
            "Should call merge on spec, got: {code}"
        );
    }

    #[test]
    fn test_generate_router_code_with_redoc_and_merge() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(other::OtherApp)];

        let result = generate_router_code(
            &metadata,
            None,
            Some("/redoc"),
            Some(quote::quote!(#spec)),
            &merge_apps,
            &[],
        );
        let code = result.to_string();

        assert!(
            code.contains("OnceLock"),
            "Should use OnceLock for merged redoc"
        );
        assert!(code.contains("redoc"), "Should contain redoc");
    }

    #[test]
    fn test_generate_router_code_with_both_docs_and_merge() {
        let metadata = CollectedMetadata::new();
        let spec = r#"{"openapi":"3.1.0"}"#;
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(merged::App)];

        let result = generate_router_code(
            &metadata,
            Some("/docs"),
            Some("/redoc"),
            Some(quote::quote!(#spec)),
            &merge_apps,
            &[],
        );
        let code = result.to_string();

        let merged_spec_count = code.matches("MERGED_SPEC").count();
        assert!(
            merged_spec_count >= 2,
            "Should have at least 2 MERGED_SPEC for docs and redoc, got: {merged_spec_count}"
        );
        let vespera_spec_count = code.matches("__VESPERA_SPEC").count();
        assert!(
            vespera_spec_count >= 1,
            "Should have __VESPERA_SPEC const, got: {vespera_spec_count}"
        );
        assert!(
            code.contains("/docs") && code.contains("/redoc"),
            "Should contain both /docs and /redoc"
        );
    }

    #[test]
    fn test_generate_router_code_with_multiple_merge_apps() {
        let metadata = CollectedMetadata::new();
        let merge_apps: Vec<syn::Path> = vec![
            syn::parse_quote!(first::App),
            syn::parse_quote!(second::App),
        ];

        let result = generate_router_code(&metadata, None, None, None, &merge_apps, &[]);
        let code = result.to_string();

        assert!(
            code.contains("first") && code.contains("second"),
            "Should reference both merge apps, got: {code}"
        );
    }

    // ===== Cron jobs =====

    #[test]
    fn test_generate_router_code_with_merge_and_cron() {
        let metadata = CollectedMetadata::new();
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(third::ThirdApp)];
        let cron_jobs = vec![CronMetadata {
            expression: "0 */5 * * * *".to_string(),
            function_name: "cleanup".to_string(),
            module_path: "tasks".to_string(),
            file_path: "src/tasks.rs".to_string(),
        }];

        let result =
            generate_router_code(&metadata, None, None, None, &merge_apps, &cron_jobs);
        let code = result.to_string();

        assert!(
            code.contains("VesperaRouter"),
            "Should use VesperaRouter for merge, got: {code}"
        );
        assert!(
            code.contains("JobScheduler"),
            "Should contain cron scheduler code, got: {code}"
        );
        assert!(
            code.contains("cleanup"),
            "Should reference cron function, got: {code}"
        );
    }

    #[test]
    fn test_generate_router_code_with_cron_no_merge() {
        let metadata = CollectedMetadata::new();
        let cron_jobs = vec![CronMetadata {
            expression: "1/10 * * * * *".to_string(),
            function_name: "heartbeat".to_string(),
            module_path: "cron::health".to_string(),
            file_path: "src/cron/health.rs".to_string(),
        }];

        let result = generate_router_code(&metadata, None, None, None, &[], &cron_jobs);
        let code = result.to_string();

        assert!(
            !code.contains("VesperaRouter"),
            "Should NOT use VesperaRouter without merge, got: {code}"
        );
        assert!(
            code.contains("JobScheduler"),
            "Should contain cron scheduler code, got: {code}"
        );
        assert!(
            code.contains("heartbeat"),
            "Should reference cron function, got: {code}"
        );
    }
}
