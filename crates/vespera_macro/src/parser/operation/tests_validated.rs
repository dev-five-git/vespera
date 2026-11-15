//! `Validated<T>` / `ValidatedWith<C, T>` extractor handling in
//! [`build_operation_from_function`]: the wrapped extractor documents its
//! request body / parameters, and ANY validated argument — wherever it sits in
//! the signature — documents the `422` validation-error response.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

/// Build the operation for `sig_src` with `CreateUser` / `Filter` known, and
/// render the parts these tests care about as one compact snapshot line each.
fn render(sig_src: &str, path: &str) -> String {
    let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
    let known: HashSet<&str> = HashSet::from(["CreateUser", "Filter"]);
    let struct_definitions: HashMap<&str, &str> = HashMap::from([
        ("CreateUser", "pub struct CreateUser { pub name: String }"),
        (
            "Filter",
            "pub struct Filter { pub q: String, pub limit: Option<u32> }",
        ),
    ]);
    let op = build_operation_from_function(
        &sig,
        path,
        &known,
        &struct_definitions,
        OperationRouteConfig::default(),
    );
    let parameters = serde_json::to_string(&op.parameters).unwrap();
    let request_body = serde_json::to_string(&op.request_body).unwrap();
    let statuses: Vec<&str> = op.responses.keys().map(String::as_str).collect();
    format!(
        "{sig_src}\nparameters: {parameters}\nrequestBody: {request_body}\nresponses: {statuses:?}"
    )
}

#[rstest]
// Audit fix: the Path scan used to `break` before reaching a later validated arg.
#[case::path_then_validated_json(
    "path_then_validated_json",
    "fn update(Path(id): Path<i64>, Validated(Json(body)): Validated<Json<CreateUser>>) -> String",
    "/users/{id}"
)]
#[case::validated_json_then_path(
    "validated_json_then_path",
    "fn update(Validated(Json(body)): Validated<Json<CreateUser>>, Path(id): Path<i64>) -> String",
    "/users/{id}"
)]
#[case::validated_path_then_validated_json(
    "validated_path_then_validated_json",
    "fn update(Validated(Path(id)): Validated<Path<i64>>, Validated(Json(body)): Validated<Json<CreateUser>>) -> String",
    "/users/{id}"
)]
// `Cached` passes the validation rejection through: still a `422`.
#[case::cached_validated_query(
    "cached_validated_query",
    "fn list(Cached(Validated(Query(f))): Cached<Validated<Query<Filter>>>) -> String",
    "/users"
)]
#[case::validated_with_json(
    "validated_with_json",
    "fn create(body: ValidatedWith<Limits, Json<CreateUser>>) -> String",
    "/users"
)]
#[case::validated_with_query(
    "validated_with_query",
    "fn search(filter: vespera::ValidatedWith<Limits, Query<Filter>>) -> String",
    "/users"
)]
#[case::path_then_validated_with_json(
    "path_then_validated_with_json",
    "fn update(Path(id): Path<i64>, body: ValidatedWith<Limits, Json<CreateUser>>) -> String",
    "/users/{id}"
)]
#[case::plain_path_and_json_has_no_422(
    "plain_path_and_json_has_no_422",
    "fn update(Path(id): Path<i64>, Json(body): Json<CreateUser>) -> String",
    "/users/{id}"
)]
fn validated_extractor_operation(#[case] name: &str, #[case] sig_src: &str, #[case] path: &str) {
    insta::assert_snapshot!(
        format!("validated_extractor_operation_{name}"),
        render(sig_src, path)
    );
}

/// The `422` content of `sig_src` with `error_status = [422]` (and, with
/// `typed`, `responses = [(422, CreateUser)]`).
fn content_422(sig_src: &str, typed: bool) -> serde_json::Value {
    let sig: syn::Signature = syn::parse_str(sig_src).unwrap();
    let known: HashSet<&str> = HashSet::from(["CreateUser"]);
    let definitions: HashMap<&str, &str> =
        HashMap::from([("CreateUser", "pub struct CreateUser { pub name: String }")]);
    let typed_responses = [(422, "CreateUser".to_string())];
    let op = build_operation_from_function(
        &sig,
        "/users",
        &known,
        &definitions,
        OperationRouteConfig {
            error_status: Some(&[422]),
            typed_responses: typed.then_some(&typed_responses[..]),
            ..OperationRouteConfig::default()
        },
    );
    serde_json::to_value(&op.responses["422"].content).unwrap()
}

/// `error_status = [422]` keeps the handler error's body AND the
/// `Validated` envelope the rejection always answers with; an explicit
/// `responses = [(422, T)]` stays the only override.
#[rstest]
#[case::text_error("Result<String, String>", false, &["application/json", "text/plain"], false)]
#[case::json_error("Result<String, Json<CreateUser>>", false, &["application/json"], true)]
#[case::bodyless_error("Result<String, StatusCode>", false, &["application/json"], false)]
#[case::typed_override("Result<String, String>", true, &["application/json"], false)]
fn error_status_422_keeps_the_validation_envelope(
    #[case] ret: &str,
    #[case] typed: bool,
    #[case] media_types: &[&str],
    #[case] any_of: bool,
) {
    let sig = format!("fn create(Validated(Json(b)): Validated<Json<CreateUser>>) -> {ret}");
    let content = content_422(&sig, typed);
    let keys: Vec<&str> = content
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, media_types, "{content}");
    let json = &content["application/json"]["schema"];
    assert_eq!(json.get("anyOf").is_some(), any_of, "{content}");
    // The envelope is there unless `responses` overrides it.
    assert_eq!(
        content.to_string().contains("\"errors\""),
        !typed,
        "{content}"
    );
}

/// An identical schema is not repeated.
#[test]
fn merged_envelope_is_not_duplicated() {
    let mut response = validation_error_response();
    merge_validation_envelope(&mut response);
    let mut empty = Response::default();
    merge_validation_envelope(&mut empty);
    let as_json = |response: &Response| serde_json::to_value(&response.content).unwrap();
    assert_eq!(as_json(&response), as_json(&validation_error_response()));
    assert_eq!(as_json(&empty), as_json(&validation_error_response()));
}
