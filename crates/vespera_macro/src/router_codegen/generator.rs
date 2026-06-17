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
            let message = format!(
                "vespera: route '{}' has unsupported HTTP method '{}'. Supported methods are GET, POST, PUT, PATCH, DELETE, HEAD, and OPTIONS.",
                route.path, route.method
            );
            router_nests.push(syn::Error::new(Span::call_site(), message).to_compile_error());
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
mod tests;
