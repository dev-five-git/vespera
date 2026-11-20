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
        deprecated: false,
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
#[case::edition_keyword("gen", Some("r#gen"))]
#[case::raw_edition_keyword("r#gen", Some("r#gen"))]
#[case::hyphen("my-file", None)]
#[case::leading_digit("2fa", None)]
#[case::path_keyword("self", None)]
#[case::trailing_space("users ", None)]
#[case::leading_space(" users", None)]
#[case::keyword_with_space("type ", None)]
fn segment_idents(#[case] name: &str, #[case] expected: Option<&str>) {
    let ident = segment_ident(name).map(|ident| ident.to_string());
    assert_eq!(ident.as_deref(), expected);
}

/// The rename suggestion for an invalid segment is itself a valid segment.
#[rstest]
#[case::hyphen("my-file", "my_file")]
#[case::leading_digit("2fa", "_2fa")]
#[case::path_keyword("self", "self_")]
#[case::underscore_only("-", "__")]
fn suggested_idents_are_valid(#[case] name: &str, #[case] expected: &str) {
    let suggestion = suggested_ident(name);
    assert_eq!(suggestion, expected);
    assert!(segment_ident(&suggestion).is_some(), "{suggestion}");
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

/// Deprecated handlers — ungated and `#[cfg]`-gated — are referenced under
/// `#[allow(deprecated)]`; the others stay plain paths.
#[test]
fn deprecated_handlers_allow_the_lint() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route("get", "routes::legacy", "old"));
    metadata.routes[0].deprecated = true;
    metadata
        .routes
        .push(route("post", "routes::legacy", "current"));
    metadata.routes[1].path = "/y".to_string();
    let mut gated = cfg_route(&["test"], "get", "/gated", "routes::gated", "old");
    gated.deprecated = true;
    metadata.cfg_routes.push(gated);
    insta::assert_snapshot!("router_deprecated_handlers", pretty(&metadata));
}

/// A gated route on an ungated handler's or a documentation route's path is
/// a `compile_error!` under its own `#[cfg]` (axum panics on the overlap
/// whenever it is compiled in); a non-overlapping gated route is mounted.
#[rstest]
#[case::handler(
    "/x",
    "/x",
    None,
    "duplicate route: `GET /x` is defined by both `routes::users::list` and `routes::gated::x`"
)]
#[case::docs_url(
    "/x",
    "/docs",
    Some("/docs"),
    "duplicate route: `GET /docs` is defined by both `docs_url` and `routes::gated::x`"
)]
#[case::param_names(
    "/x/{id}",
    "/x/{key}",
    None,
    "conflicting route parameters: `/x/{key}`"
)]
#[case::no_overlap("/x", "/y", Some("/docs"), "")]
fn gated_route_overlaps_are_gated_compile_errors(
    #[case] ungated_path: &str,
    #[case] gated_path: &str,
    #[case] docs_url: Option<&str>,
    #[case] message: &str,
) {
    let mut metadata = CollectedMetadata::new();
    let mut ungated = route("get", "routes::users", "list");
    ungated.path = ungated_path.to_string();
    metadata.routes.push(ungated);
    metadata.cfg_routes.push(cfg_route(
        &["unix"],
        "get",
        gated_path,
        "routes::gated",
        "x",
    ));
    let body = generate_router_code(&metadata, docs_url, None, None, None, &[], &[]);
    syn::parse2::<syn::File>(quote!(fn create_app() { #body })).expect("valid fn");
    let code = body.to_string();
    if message.is_empty() {
        assert!(!code.contains("compile_error"), "{code}");
    } else {
        assert!(
            code.contains("# [cfg (all (unix))] :: core :: compile_error !"),
            "{code}"
        );
        assert!(code.contains(message), "{code}");
    }
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
    let body = generate_router_code(&metadata, None, None, None, None, &[], &[]);
    // The error must survive parsing: spliced into the `.route(..)` chain it
    // would be a syntax error that hides the message.
    syn::parse2::<syn::File>(quote!(fn create_app() { #body })).expect("valid fn");
    let code = body.to_string();
    assert!(code.contains("compile_error"), "{code}");
    assert!(code.contains(message), "{code}");
}

/// An unsupported method on a `#[cfg]`-gated route is a `compile_error!`
/// under that route's own `#[cfg]` (round-29 macrorest-4 L-1): with the
/// module compiled out the error is compiled out with it.
#[test]
fn gated_unsupported_method_error_carries_the_route_cfg() {
    let mut metadata = CollectedMetadata::new();
    metadata
        .cfg_routes
        .push(cfg_route(&["unix"], "connect", "/t", "routes::t", "t"));
    let code = generate_router_code(&metadata, None, None, None, None, &[], &[]).to_string();
    assert!(
        code.contains("[cfg (all (unix))] :: core :: compile_error"),
        "{code}"
    );
}

/// A documented route's error between two valid routes is a statement ahead
/// of the chain, which keeps the valid routes.
#[test]
fn documented_route_error_is_a_statement_ahead_of_the_chain() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route("get", "routes::users", "list"));
    metadata
        .routes
        .push(route("get", "routes::my-file", "list"));
    metadata
        .routes
        .push(route("post", "routes::users", "create"));
    insta::assert_snapshot!("router_route_error_statement", pretty(&metadata));
}

#[test]
fn cron_job_in_non_identifier_file_is_a_compile_error() {
    let cron_jobs = [CronMetadata {
        expression: "0 * * * * *".to_string(),
        function_name: "tick".to_string(),
        module_path: "cron::my-jobs".to_string(),
        file_path: "src/cron/my-jobs.rs".to_string(),
        cfg: Vec::new(),
        options: CronOptions::default(),
    }];
    let code = generate_cron_scheduler_code(&cron_jobs).to_string();
    assert!(
        code.contains("'src/cron/my-jobs.rs' maps to the module path segment `my-jobs`"),
        "{code}"
    );
}
