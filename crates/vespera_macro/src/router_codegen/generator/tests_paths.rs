//! Handler paths: raw/keyword identifiers, non-identifier file names and
//! `#[cfg]`-gated routes.

use quote::quote;
use rstest::rstest;

use super::*;
use crate::metadata::{CfgRoute, CronOptions, RouteMetadata};

fn route(method: &str, module_path: &str, function_name: &str) -> RouteMetadata {
    RouteMetadata {
        method: method.to_string(),
        path: "/x".to_string(),
        function_name: function_name.to_string(),
        module_path: module_path.to_string(),
        file_path: "src/routes/x.rs".to_string(),
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
    }
}

fn cfg_route(
    cfg: &[&str],
    method: &str,
    path: &str,
    module_path: &str,
    function_name: &str,
) -> CfgRoute {
    CfgRoute {
        cfg: cfg.iter().map(ToString::to_string).collect(),
        method: method.to_string(),
        path: path.to_string(),
        module_path: module_path.to_string(),
        function_name: function_name.to_string(),
        file_path: format!("src/{}.rs", module_path.replace("::", "/")),
    }
}

fn pretty(metadata: &CollectedMetadata) -> String {
    let body = generate_router_code(metadata, None, None, None, None, &[], &[]);
    let file: syn::File = syn::parse2(quote!(fn create_app() { #body })).expect("valid fn");
    prettyplease::unparse(&file)
}

#[rstest]
#[case::plain("users", Some("users"))]
#[case::keyword("type", Some("r#type"))]
#[case::raw_keyword("r#match", Some("r#match"))]
#[case::raw_plain("r#users", Some("users"))]
#[case::hyphen("my-file", None)]
#[case::leading_digit("2fa", None)]
#[case::path_keyword("self", None)]
fn segment_idents(#[case] name: &str, #[case] expected: Option<&str>) {
    let ident = segment_ident(name).map(|ident| ident.to_string());
    assert_eq!(ident.as_deref(), expected);
}

/// Keyword module / handler names become raw identifiers; gated routes are
/// mounted under their `#[cfg]` after the ungated chain.
#[test]
fn keyword_and_gated_routes() {
    let mut metadata = CollectedMetadata::new();
    metadata
        .routes
        .push(route("get", "routes::type", "r#match"));
    metadata.cfg_routes.push(cfg_route(
        &["feature = \"admin\""],
        "post",
        "/admin",
        "routes::admin",
        "panel",
    ));
    metadata.cfg_routes.push(cfg_route(
        &["feature = \"admin\"", "unix"],
        "get",
        "/admin/stats/{id}",
        "routes::admin::stats",
        "stats",
    ));
    insta::assert_snapshot!("router_keyword_and_gated_routes", pretty(&metadata));
}

#[test]
fn ungated_router_stays_a_method_chain() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route("get", "routes::users", "list"));
    insta::assert_snapshot!("router_ungated_method_chain", pretty(&metadata));
}

/// A non-identifier file name or an unsupported method is a `compile_error!`
/// naming the culprit, for documented and gated routes and for cron jobs.
#[rstest]
#[case::route_file(
    route("get", "routes::my-file", "list"),
    None,
    "'src/routes/x.rs' maps to the module path segment `my-file`"
)]
#[case::gated_file(route("get", "routes::ok", "ok"), Some(cfg_route(&["unix"], "get", "/a", "routes::2fa", "a")), "'src/routes/2fa.rs' maps to the module path segment `2fa`")]
#[case::gated_method(route("get", "routes::ok", "ok"), Some(cfg_route(&["unix"], "connect", "/t", "routes::t", "t")), "route '/t' has unsupported HTTP method 'connect'")]
fn unaddressable_handlers_are_compile_errors(
    #[case] documented: RouteMetadata,
    #[case] gated: Option<CfgRoute>,
    #[case] message: &str,
) {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(documented);
    metadata.cfg_routes.extend(gated);
    let code = generate_router_code(&metadata, None, None, None, None, &[], &[]).to_string();
    assert!(code.contains("compile_error"), "{code}");
    assert!(code.contains(message), "{code}");
}

#[test]
fn cron_job_in_non_identifier_file_is_a_compile_error() {
    let cron_jobs = [CronMetadata {
        expression: "0 * * * * *".to_string(),
        function_name: "tick".to_string(),
        module_path: "cron::my-jobs".to_string(),
        file_path: "src/cron/my-jobs.rs".to_string(),
        options: CronOptions::default(),
    }];
    let code = generate_cron_scheduler_code(&cron_jobs).to_string();
    assert!(
        code.contains("'src/cron/my-jobs.rs' maps to the module path segment `my-jobs`"),
        "{code}"
    );
}
