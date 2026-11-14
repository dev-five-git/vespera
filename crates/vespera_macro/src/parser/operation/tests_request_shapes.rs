//! Request extractor shapes: axum-extra's `WithRejection` / `Cached` /
//! `OptionalQuery`, `Option<Path<T>>`, query structs without documentable
//! fields, `Option<T>` query fields read by a custom deserializer, path
//! maps / sequences and `#[serde(from = "X")]` path / query containers.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

fn operation(sig: &str, path: &str) -> Operation {
    let sig: syn::Signature = syn::parse_str(sig).unwrap();
    let known = HashSet::from(["Filter", "CreateUser", "Kind"]);
    let definitions = HashMap::from([
        (
            "Filter",
            "pub struct Filter { pub q: String, pub page: Option<u32> }",
        ),
        ("CreateUser", "pub struct CreateUser { pub name: String }"),
        ("Ids", "pub struct Ids { pub id: u64, pub slug: String }"),
        ("Empty", "pub struct Empty {}"),
        (
            "Skipped",
            "pub struct Skipped { #[serde(skip)] pub internal: String }",
        ),
        (
            "CatchAll",
            "pub struct CatchAll { #[serde(flatten)] pub rest: HashMap<String, String> }",
        ),
        ("Wrapper", "pub struct Wrapper(pub Filter);"),
        (
            "Kind",
            "#[serde(tag = \"kind\")] pub enum Kind { A { a: u32 }, B { b: String } }",
        ),
        (
            "Window",
            r#"pub struct Window {
                #[serde(deserialize_with = "parse_unix")] pub since: Option<i64>,
                #[serde(with = "csv")] pub tags: Option<Vec<String>>,
                #[serde(deserialize_with = "parse_unix", default)] pub until: Option<i64>,
                #[serde(serialize_with = "fmt")] pub plain: Option<u32>,
            }"#,
        ),
        (
            "Converted",
            r#"#[serde(from = "Filter")] pub struct Converted { pub internal: Vec<u8> }"#,
        ),
        (
            "TryConverted",
            r#"#[serde(try_from = "Ids")] pub struct TryConverted { pub raw: String }"#,
        ),
        (
            "TupleConverted",
            r#"#[serde(from = "(u64, String)")] pub struct TupleConverted { pub raw: String }"#,
        ),
        (
            "Looping",
            r#"#[serde(from = "Looping")] pub struct Looping { pub raw: String }"#,
        ),
    ]);
    build_operation_from_function(
        &sig,
        path,
        &known,
        &definitions,
        OperationRouteConfig::default(),
    )
}

#[rstest]
// 1: `axum_extra::extract::OptionalQuery<T>` — every parameter optional.
#[case::optional_query("optional_query", "async fn f(q: OptionalQuery<Filter>)", "/x")]
#[case::optional_query_qualified(
    "optional_query_qualified",
    "async fn f(OptionalQuery(q): axum_extra::extract::OptionalQuery<Filter>)",
    "/x"
)]
#[case::optional_query_enum("optional_query_enum", "async fn f(kind: OptionalQuery<Kind>)", "/x")]
// 2: `WithRejection<E, R>` / `Cached<E>` document `E`.
#[case::with_rejection_json(
    "with_rejection_json",
    "async fn f(WithRejection(Json(b), _): WithRejection<Json<CreateUser>, ApiError>)",
    "/x"
)]
#[case::with_rejection_query(
    "with_rejection_query",
    "async fn f(q: axum_extra::extract::WithRejection<Query<Filter>, ApiError>)",
    "/x"
)]
#[case::with_rejection_path(
    "with_rejection_path",
    "async fn f(p: WithRejection<Path<u64>, ApiError>)",
    "/x/{id}"
)]
#[case::cached_header(
    "cached_header",
    "async fn f(ua: Cached<TypedHeader<UserAgent>>)",
    "/x"
)]
#[case::with_rejection_validated(
    "with_rejection_validated",
    "async fn f(b: WithRejection<Validated<Json<CreateUser>>, ApiError>)",
    "/x"
)]
// 3: `Option<T>` with a custom deserializer and no `default` is required.
#[case::custom_deserializer_option(
    "custom_deserializer_option",
    "async fn f(q: Query<Window>)",
    "/x"
)]
// 7: `Option<Path<T>>` still types the path segments.
#[case::option_path_struct(
    "option_path_struct",
    "async fn f(p: Option<Path<Ids>>)",
    "/x/{id}/{slug}"
)]
#[case::option_path_scalar("option_path_scalar", "async fn f(p: Option<Path<u64>>)", "/x/{id}")]
// 8: a known struct with nothing documentable is no fake binding parameter.
#[case::empty_struct("empty_struct", "async fn f(empty: Query<Empty>)", "/x")]
#[case::skipped_struct("skipped_struct", "async fn f(skipped: Query<Skipped>)", "/x")]
#[case::flatten_map_only("flatten_map_only", "async fn f(rest: Query<CatchAll>)", "/x")]
#[case::tuple_struct("tuple_struct", "async fn f(w: Query<Wrapper>)", "/x")]
#[case::known_enum("known_enum", "async fn f(kind: Query<Kind>)", "/x")]
// 9: `Path<HashMap<..>>` / `Path<Vec<..>>` type each segment.
#[case::path_map("path_map", "async fn f(p: Path<HashMap<String, u64>>)", "/x/{id}")]
#[case::path_map_multi(
    "path_map_multi",
    "async fn f(p: Path<BTreeMap<String, String>>)",
    "/x/{a}/{b}"
)]
#[case::path_vec("path_vec", "async fn f(p: Path<Vec<u32>>)", "/x/{a}/{b}")]
#[case::path_map_one_arg("path_map_one_arg", "async fn f(p: Path<HashMap<String>>)", "/x/{a}")]
#[case::path_generic_scalar("path_generic_scalar", "async fn f(p: Path<NonZero<u64>>)", "/x/{a}")]
// 10: container `from` / `try_from` documents the source type.
#[case::query_from("query_from", "async fn f(q: Query<Converted>)", "/x")]
#[case::path_try_from("path_try_from", "async fn f(p: Path<TryConverted>)", "/x/{id}/{slug}")]
#[case::path_from_tuple(
    "path_from_tuple",
    "async fn f(p: Path<TupleConverted>)",
    "/x/{id}/{slug}"
)]
#[case::path_from_cycle("path_from_cycle", "async fn f(p: Path<Looping>)", "/x/{raw}")]
#[case::query_from_cycle("query_from_cycle", "async fn f(q: Query<Looping>)", "/x")]
fn request_shapes(#[case] name: &str, #[case] sig: &str, #[case] path: &str) {
    let op = operation(sig, path);
    let summary = serde_json::json!({
        "parameters": op.parameters,
        "requestBody": op.request_body,
        "has422": op.responses.contains_key("422"),
    });
    insta::assert_snapshot!(
        format!("request_shapes_{name}"),
        serde_json::to_string_pretty(&summary).unwrap()
    );
}

/// `error_status` copies the first ERROR response's media: a `NoContent`
/// (`204`) or `Redirect` (`3XX`) success entry is not an error.
#[rstest]
#[case::no_content("async fn f() -> Result<NoContent, Json<CreateUser>>")]
#[case::redirect("async fn f() -> Result<Redirect, Json<CreateUser>>")]
fn error_status_copies_the_error_media(#[case] sig: &str) {
    let sig: syn::Signature = syn::parse_str(sig).unwrap();
    let known = HashSet::from(["CreateUser"]);
    let definitions = HashMap::from([("CreateUser", "pub struct CreateUser { pub name: String }")]);
    let op = build_operation_from_function(
        &sig,
        "/x",
        &known,
        &definitions,
        OperationRouteConfig {
            error_status: Some(&[404]),
            ..OperationRouteConfig::default()
        },
    );
    let not_found = serde_json::to_string(&op.responses["404"]).unwrap();
    assert_eq!(
        not_found,
        r##"{"description":"Error response","content":{"application/json":{"schema":{"$ref":"#/components/schemas/CreateUser"}}}}"##
    );
}
