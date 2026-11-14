//! Request extractors behind `Result<E, Rejection>` (the handler takes the
//! rejection itself) document exactly like the bare extractor — still
//! required, unlike `Option<E>` — and `Path<(..)>` tuples longer than the
//! route's own parameters (a `Router::nest("/{tenant}", ..)` prefix fills
//! the leading elements) type the route's segments from the END.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

fn operation(sig: &str, path: &str, form_in_query: bool) -> Operation {
    let sig: syn::Signature = syn::parse_str(sig).unwrap();
    let known = HashSet::from(["Filter", "CreateUser"]);
    let definitions = HashMap::from([
        (
            "Filter",
            "pub struct Filter { pub q: String, pub page: Option<u32> }",
        ),
        ("CreateUser", "pub struct CreateUser { pub name: String }"),
    ]);
    build_operation_from_function(
        &sig,
        path,
        &known,
        &definitions,
        OperationRouteConfig {
            form_in_query,
            ..OperationRouteConfig::default()
        },
    )
}

#[rstest]
#[case::result_json(
    "result_json",
    "async fn f(body: Result<Json<CreateUser>, JsonRejection>) -> String",
    "/users",
    false
)]
#[case::std_result_json(
    "std_result_json",
    "async fn f(body: std::result::Result<axum::Json<CreateUser>, JsonRejection>) -> String",
    "/users",
    false
)]
#[case::core_result_json(
    "core_result_json",
    "async fn f(body: ::core::result::Result<Json<CreateUser>, JsonRejection>) -> String",
    "/users",
    false
)]
#[case::result_validated_json(
    "result_validated_json",
    "async fn f(body: Result<Validated<Json<CreateUser>>, ValidationRejection>) -> String",
    "/users",
    false
)]
#[case::result_query(
    "result_query",
    "async fn f(q: Result<Query<Filter>, QueryRejection>) -> String",
    "/users",
    false
)]
#[case::result_form_on_get(
    "result_form_on_get",
    "async fn f(form: Result<Form<Filter>, FormRejection>) -> String",
    "/users",
    true
)]
#[case::option_form_on_get(
    "option_form_on_get",
    "async fn f(form: Option<Form<Filter>>) -> String",
    "/users",
    true
)]
#[case::result_path_tuple(
    "result_path_tuple",
    "async fn f(p: Result<Path<(u64, String)>, PathRejection>) -> String",
    "/users/{id}/{slug}",
    false
)]
#[case::result_typed_header(
    "result_typed_header",
    "async fn f(ua: Result<TypedHeader<UserAgent>, TypedHeaderRejection>) -> String",
    "/users",
    false
)]
#[case::result_alias_single_argument(
    "result_alias_single_argument",
    "async fn f(body: io::Result<Json<CreateUser>>) -> String",
    "/users",
    false
)]
#[case::non_std_result_path(
    "non_std_result_path",
    "async fn f(body: my::result::Result<Json<CreateUser>, JsonRejection>) -> String",
    "/users",
    false
)]
#[case::nest_prefix_tuple(
    "nest_prefix_tuple",
    "async fn f(Path((tenant, id)): Path<(String, u64)>) -> String",
    "/{id}",
    false
)]
#[case::nest_prefix_tuple_two_own(
    "nest_prefix_tuple_two_own",
    "async fn f(Path((tenant, id, flag)): Path<(String, u64, bool)>) -> String",
    "/{id}/{flag}",
    false
)]
#[case::prefixed_segment_param(
    "prefixed_segment_param",
    "async fn f(Path(name): Path<u32>) -> String",
    "/files/file-{name}",
    false
)]
#[case::escaped_braces(
    "escaped_braces",
    "async fn f(Path(id): Path<u32>) -> String",
    "/lit/{{x}}/{id}",
    false
)]
fn wrapped_extractors_document_like_the_bare_extractor(
    #[case] name: &str,
    #[case] sig: &str,
    #[case] path: &str,
    #[case] form_in_query: bool,
) {
    let op = operation(sig, path, form_in_query);
    let summary = serde_json::json!({
        "parameters": op.parameters,
        "requestBody": op.request_body,
        "has422": op.responses.contains_key("422"),
    });
    insta::assert_snapshot!(
        format!("extractor_wrappers_{name}"),
        serde_json::to_string_pretty(&summary).unwrap()
    );
}
