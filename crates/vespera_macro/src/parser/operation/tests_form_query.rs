//! axum's `Form<T>` reads the query string on GET / HEAD: documented as
//! query parameters there, as a form body elsewhere.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

fn operation(sig: &str, form_in_query: bool) -> Operation {
    let sig: syn::Signature = syn::parse_str(sig).unwrap();
    let known = HashSet::from(["Search"]);
    let definitions = HashMap::from([(
        "Search",
        "pub struct Search { pub q: String, pub page: Option<u32> }",
    )]);
    build_operation_from_function(
        &sig,
        "/search",
        &known,
        &definitions,
        OperationRouteConfig {
            form_in_query,
            ..OperationRouteConfig::default()
        },
    )
}

#[rstest]
#[case::get_form("get_form", "async fn f(Form(s): Form<Search>) -> String", true)]
#[case::get_validated_form(
    "get_validated_form",
    "async fn f(Validated(Form(s)): Validated<Form<Search>>) -> String",
    true
)]
#[case::get_qualified_form(
    "get_qualified_form",
    "async fn f(form: axum::extract::Form<Search>) -> String",
    true
)]
#[case::get_receiver_and_reference(
    "get_receiver_and_reference",
    "async fn f(&self, headers: &HeaderMap, Form(s): Form<Search>) -> String",
    true
)]
#[case::post_form("post_form", "async fn f(Form(s): Form<Search>) -> String", false)]
#[case::get_json("get_json", "async fn f(Json(s): Json<Search>) -> String", true)]
fn form_location_follows_the_method(
    #[case] name: &str,
    #[case] sig: &str,
    #[case] form_in_query: bool,
) {
    let op = operation(sig, form_in_query);
    let summary = serde_json::json!({
        "parameters": op.parameters,
        "requestBody": op.request_body,
    });
    insta::assert_snapshot!(
        format!("form_query_{name}"),
        serde_json::to_string_pretty(&summary).unwrap()
    );
}
