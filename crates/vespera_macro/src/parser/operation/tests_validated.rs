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
