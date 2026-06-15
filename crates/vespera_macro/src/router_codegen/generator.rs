use proc_macro2::Span;
use quote::quote;
use vespera_core::route::HttpMethod;

use crate::{
    metadata::{CollectedMetadata, CronMetadata},
    method::http_method_to_token_stream,
};

use super::docs::{REDOC_HTML, SWAGGER_UI_HTML, generate_docs_route_tokens};

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

        // Should generate empty router
        // quote! generates "vespera :: axum :: Router :: new ()" format
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

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(
            code.contains("Router") && code.contains("new"),
            "Code should contain Router::new(), got: {code}"
        );

        // Check route method
        assert!(
            code.contains(expected_method),
            "Code should contain method: {expected_method}, got: {code}"
        );

        // Check route path
        assert!(
            code.contains(expected_path),
            "Code should contain path: {expected_path}, got: {code}"
        );

        // Check function path (quote! adds spaces, so we check for parts)
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
            "Should have 3 route calls, got: {route_count}, code: {code}"
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
            "Should have 2 routes, got: {route_count}, code: {code}"
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

        // Check router initialization (quote! generates "vespera :: axum :: Router :: new ()")
        assert!(code.contains("Router") && code.contains("new"));

        // Check route is present
        assert!(code.contains("get_users"));

        // Module path should not have double colons
        assert!(!code.contains("::users::users"));

        drop(temp_dir);
    }

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
    fn test_generate_router_code_unknown_http_method() {
        // Test lines 337-340: route with unknown HTTP method is skipped in router codegen
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
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let result = generate_router_code(&metadata, None, None, None, &[], &[]);
        let code = result.to_string();

        // Router should be generated but without any route calls
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
        // Test that unknown methods are skipped while valid routes are still generated
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
        // Inject an additional route with invalid method
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
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let result = generate_router_code(&metadata, None, None, None, &[], &[]);
        let code = result.to_string();

        // Valid route should be present
        assert!(
            code.contains("get_users"),
            "Valid route should be present, got: {code}"
        );
        // Invalid route should be skipped
        assert!(
            !code.contains("connect_handler"),
            "Invalid method route should be skipped, got: {code}"
        );

        drop(temp_dir);
    }

    #[test]
    fn test_generate_router_code_with_merge_apps() {
        let metadata = CollectedMetadata::new();
        let merge_apps: Vec<syn::Path> = vec![syn::parse_quote!(third::ThirdApp)];

        let result = generate_router_code(&metadata, None, None, None, &merge_apps, &[]);
        let code = result.to_string();

        // Should use VesperaRouter instead of plain Router
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

        // Should have merge code for docs
        assert!(
            code.contains("OnceLock"),
            "Should use OnceLock for merged docs, got: {code}"
        );
        assert!(
            code.contains("MERGED_SPEC"),
            "Should have MERGED_SPEC, got: {code}"
        );
        // quote! generates "merged . merge" with spaces
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

        // Should have merge code for redoc
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

        // Both docs should have merge code
        // Count MERGED_SPEC occurrences - should appear in docs and redoc handlers
        let merged_spec_count = code.matches("MERGED_SPEC").count();
        assert!(
            merged_spec_count >= 2,
            "Should have at least 2 MERGED_SPEC for docs and redoc, got: {merged_spec_count}"
        );
        // __VESPERA_SPEC should appear exactly once (the const declaration)
        let vespera_spec_count = code.matches("__VESPERA_SPEC").count();
        assert!(
            vespera_spec_count >= 1,
            "Should have __VESPERA_SPEC const, got: {vespera_spec_count}"
        );
        // Both docs_url and redoc_url should be present
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

        // Should reference both apps
        assert!(
            code.contains("first") && code.contains("second"),
            "Should reference both merge apps, got: {code}"
        );
    }

    // ========== Tests for generate_router_code with cron jobs ==========

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

        let result = generate_router_code(&metadata, None, None, None, &merge_apps, &cron_jobs);
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
