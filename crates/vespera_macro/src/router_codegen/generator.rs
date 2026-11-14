use proc_macro2::Span;
use quote::quote;
use vespera_core::route::HttpMethod;

use crate::{
    metadata::{CfgRoute, CollectedMetadata, CronMetadata},
    method::http_method_to_token_stream,
};

use super::docs::{DocsPage, generate_docs_route_tokens, generate_spec_route_tokens};

/// `name` as a path segment: a plain identifier, or the raw `r#name` form
/// for a keyword (`type.rs` is `mod r#type;`, `fn r#match` is recorded as
/// `r#match`). `None` when it is neither (`my-file`, `2fa`, `self`).
fn segment_ident(name: &str) -> Option<syn::Ident> {
    let name = name.strip_prefix("r#").unwrap_or(name);
    let mut ident = syn::parse_str::<syn::Ident>(name)
        .or_else(|_| syn::parse_str::<syn::Ident>(&format!("r#{name}")))
        .ok()?;
    ident.set_span(Span::call_site());
    Some(ident)
}

/// Build `crate::<module_path>::<function_name>` as a `syn::Path`.
/// Empty segments inside `module_path` (from accidental `::` doubling)
/// are dropped, matching the existing behaviour of both call sites.
///
/// A segment that is no Rust identifier (a route file named `my-file.rs`)
/// is a `compile_error!` naming `file_path` instead of a macro panic.
fn crate_path_to(
    module_path: &str,
    function_name: &str,
    file_path: &str,
) -> Result<syn::Path, proc_macro2::TokenStream> {
    let mut segments: syn::punctuated::Punctuated<syn::PathSegment, syn::Token![::]> =
        syn::punctuated::Punctuated::new();
    segments.push(syn::PathSegment::from(syn::Ident::new(
        "crate",
        Span::call_site(),
    )));
    for name in module_path
        .split("::")
        .filter(|s| !s.is_empty())
        .chain(std::iter::once(function_name))
    {
        let Some(ident) = segment_ident(name) else {
            let message = format!(
                "vespera: '{file_path}' maps to the module path segment `{name}`, which is not a valid Rust identifier. Rename the file (or folder) to snake_case, e.g. `{}`.",
                name.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
            );
            return Err(syn::Error::new(Span::call_site(), message).to_compile_error());
        };
        segments.push(syn::PathSegment::from(ident));
    }
    Ok(syn::Path {
        leading_colon: None,
        segments,
    })
}

/// The `.route(..)` arguments `"/path", get(crate::module::handler)`, or the
/// `compile_error!` for an unsupported method / unaddressable handler.
fn route_args(
    method: &str,
    path: &str,
    module_path: &str,
    function_name: &str,
    file_path: &str,
) -> Result<proc_macro2::TokenStream, proc_macro2::TokenStream> {
    let Ok(http_method) = HttpMethod::try_from(method) else {
        let message = format!(
            "vespera: route '{path}' has unsupported HTTP method '{method}'. Supported methods are GET, POST, PUT, PATCH, DELETE, HEAD, and OPTIONS."
        );
        return Err(syn::Error::new(Span::call_site(), message).to_compile_error());
    };
    let method_path = http_method_to_token_stream(http_method);
    let call_path = crate_path_to(module_path, function_name, file_path)?;
    Ok(quote!(#path, #method_path(#call_path)))
}

/// `#[cfg(all(..))] let __vespera_router = __vespera_router.route(..);` per
/// [`CfgRoute`]: the compiler keeps the routes whose module was compiled in.
fn gated_route_statements(cfg_routes: &[CfgRoute]) -> Vec<proc_macro2::TokenStream> {
    cfg_routes
        .iter()
        .map(|route| {
            let predicates = route.cfg.iter().map(|predicate| {
                predicate
                    .parse::<proc_macro2::TokenStream>()
                    .unwrap_or_default()
            });
            match route_args(
                &route.method,
                &route.path,
                &route.module_path,
                &route.function_name,
                &route.file_path,
            ) {
                Ok(args) => quote! {
                    #[cfg(all(#(#predicates),*))]
                    let __vespera_router = __vespera_router.route(#args);
                },
                Err(error) => error,
            }
        })
        .collect()
}

/// Generate the once-per-process cron scheduler start from collected cron metadata.
fn generate_cron_scheduler_code(cron_jobs: &[CronMetadata]) -> proc_macro2::TokenStream {
    if cron_jobs.is_empty() {
        return quote!();
    }

    let jobs = cron_jobs.iter().map(|cron| {
        let expression = &cron.expression;
        let call_path = match crate_path_to(&cron.module_path, &cron.function_name, &cron.file_path)
        {
            Ok(path) => path,
            Err(error) => return error,
        };
        // `module::function` names the job in scheduler logs (panics, skips).
        let name = if cron.module_path.is_empty() {
            cron.function_name.clone()
        } else {
            format!("{}::{}", cron.module_path, cron.function_name)
        };
        // Non-default options only: a plain UTC / overlap-allowed job keeps
        // the pre-option expansion.
        let timezone = cron
            .options
            .timezone
            .as_ref()
            .map(|tz| quote!(.with_timezone(#tz)));
        let overlap = cron
            .options
            .skip_overlap
            .then(|| quote!(.with_overlap(vespera::cron::Overlap::Skip)));
        quote! {
            vespera::cron::CronJob::new(#expression, || ::std::boxed::Box::pin(#call_path()))
                .with_name(#name)
                #timezone
                #overlap
        }
    });

    // One guard per `vespera!` invocation: re-building the router (tests,
    // `jni_app!`, repeated `create_app()` calls) never stacks a second set of
    // schedulers, and `start_once` copes with "no Tokio runtime yet".
    quote! {
        {
            static __VESPERA_CRON_ONCE: ::std::sync::Once = ::std::sync::Once::new();
            vespera::cron::start_once(&__VESPERA_CRON_ONCE, [#(#jobs),*]);
        }
    }
}

/// Generate Axum router code from collected metadata
///
/// `docs_url` / `redoc_url` / `spec_url` each add a GET route serving the
/// spec in `spec_tokens` (Swagger UI page, ReDoc page, raw JSON).
#[allow(clippy::too_many_lines)]
pub fn generate_router_code(
    metadata: &CollectedMetadata,
    docs_url: Option<&str>,
    redoc_url: Option<&str>,
    spec_url: Option<&str>,
    spec_tokens: Option<proc_macro2::TokenStream>,
    merge_apps: &[syn::Path],
    cron_jobs: &[CronMetadata],
) -> proc_macro2::TokenStream {
    let mut router_nests = Vec::new();

    for route in &metadata.routes {
        router_nests.push(
            match route_args(
                &route.method,
                &route.path,
                &route.module_path,
                &route.function_name,
                &route.file_path,
            ) {
                Ok(args) => quote!(.route(#args)),
                Err(error) => error,
            },
        );
    }

    // Check if we need to merge specs at runtime
    let has_merge = !merge_apps.is_empty();

    // Generate merge code once, reuse in the docs_url / redoc_url / spec_url routes
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

    let docs_spec_expr = if has_merge {
        quote! { __vespera_merged_spec() }
    } else {
        quote! { __VESPERA_SPEC }
    };

    if let Some(docs_url) = docs_url {
        router_nests.push(generate_docs_route_tokens(
            docs_url,
            DocsPage::SwaggerUi,
            &docs_spec_expr,
        ));
    }

    if let Some(redoc_url) = redoc_url {
        router_nests.push(generate_docs_route_tokens(
            redoc_url,
            DocsPage::Redoc,
            &docs_spec_expr,
        ));
    }

    if let Some(spec_url) = spec_url {
        router_nests.push(generate_spec_route_tokens(spec_url, &docs_spec_expr));
    }

    // `#[cfg]`-gated routes need statements: an attribute cannot gate one
    // `.route(..)` call of a method chain.
    let gated = gated_route_statements(&metadata.cfg_routes);
    let router_expr = if gated.is_empty() {
        quote!(vespera::axum::Router::new() #( #router_nests )*)
    } else {
        quote!({
            let __vespera_router = vespera::axum::Router::new() #( #router_nests )*;
            #( #gated )*
            __vespera_router
        })
    };

    let needs_spec_const =
        spec_tokens.is_some() && (docs_url.is_some() || redoc_url.is_some() || spec_url.is_some());
    let cron_code = generate_cron_scheduler_code(cron_jobs);

    if needs_spec_const {
        let spec_expr = spec_tokens.unwrap();
        if merge_apps.is_empty() {
            quote! {
                {
                    const __VESPERA_SPEC: &str = #spec_expr;
                    #cron_code
                    #router_expr
                }
            }
        } else {
            quote! {
                {
                    const __VESPERA_SPEC: &str = #spec_expr;
                    fn __vespera_merged_spec() -> &'static str {
                        static MERGED_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                        MERGED_SPEC.get_or_init(|| {
                            // The base spec is Vespera-generated and expected to parse; on the
                            // unreachable drift where parse or re-serialization fails, fall back
                            // to serving the un-merged base spec instead of panicking inside this
                            // request handler — the docs page still renders.
                            let Ok(mut merged) =
                                vespera::serde_json::from_str::<vespera::OpenApi>(__VESPERA_SPEC)
                            else {
                                return __VESPERA_SPEC.to_string();
                            };
                            #(#merge_spec_code)*
                            vespera::serde_json::to_string(&merged)
                                .unwrap_or_else(|_| __VESPERA_SPEC.to_string())
                        })
                    }
                    #cron_code
                    vespera::VesperaRouter::new(
                        #router_expr,
                        vec![#( #merge_apps::router ),*]
                    )
                }
            }
        }
    } else if merge_apps.is_empty() {
        if cron_jobs.is_empty() {
            quote! {
                #router_expr
            }
        } else {
            quote! {
                {
                    #cron_code
                    #router_expr
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
                    #router_expr,
                    vec![#( #merge_apps::router ),*]
                )
            }
        } else {
            quote! {
                {
                    #cron_code
                    vespera::VesperaRouter::new(
                        #router_expr,
                        vec![#( #merge_apps::router ),*]
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_docs;

#[cfg(test)]
mod tests_paths;
