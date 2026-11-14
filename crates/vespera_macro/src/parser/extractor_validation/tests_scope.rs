//! Only paths that name the route module's own items are matched against its
//! local types (`serde_json::Value` is not a local `struct Value`), and
//! `Path<T>` / `OptionalQuery<T>` / `WithRejection` / `Cached` extractors are
//! checked like `Json<T>`.

use rstest::rstest;

use super::*;
use crate::metadata::{CollectedMetadata, RouteMetadata};

fn run(src: &str) -> syn::Result<()> {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(RouteMetadata {
        method: "get".to_string(),
        path: "/x".to_string(),
        function_name: "handler".to_string(),
        module_path: "routes::x".to_string(),
        file_path: "x.rs".to_string(),
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
    });
    let ast: syn::File = syn::parse_str(src).expect("source parses");
    let file_cache = HashMap::from([("x.rs".to_string(), ast)]);
    check_extractors(&metadata, &file_cache)
}

#[rstest]
#[case::foreign_value("pub fn handler(Json(v): Json<serde_json::Value>) {}")]
#[case::foreign_nested("pub fn handler(Json(v): Json<Vec<serde_json::Value>>) {}")]
#[case::other_crate_module("pub fn handler(Json(v): Json<crate::models::Value>) {}")]
#[case::leading_colon("pub fn handler(Json(v): Json<::Value>) {}")]
#[case::qself("pub fn handler(Json(v): Json<<T as Tr>::Value>) {}")]
#[case::path_tuple("pub fn handler(Path((a, b)): Path<(Value, u32)>) {}")]
fn foreign_paths_are_not_local_types(#[case] handler: &str) {
    let src = format!("pub struct Value {{ pub a: i32 }} {handler}");
    assert!(run(&src).is_ok(), "{handler}");
}

#[rstest]
#[case::bare("pub fn handler(Json(v): Json<Value>) {}", "Json<Value>")]
#[case::self_path("pub fn handler(Json(v): Json<self::Value>) {}", "Json<Value>")]
#[case::crate_path(
    "pub fn handler(Json(v): Json<crate::routes::x::Value>) {}",
    "Json<Value>"
)]
#[case::path_struct("pub fn handler(Path(v): Path<Value>) {}", "Path<Value>")]
#[case::option_path("pub fn handler(v: Option<Path<Value>>) {}", "Path<Value>")]
#[case::optional_query(
    "pub fn handler(OptionalQuery(v): OptionalQuery<Value>) {}",
    "OptionalQuery<Value>"
)]
#[case::with_rejection(
    "pub fn handler(v: WithRejection<Json<Value>, ApiError>) {}",
    "Json<Value>"
)]
#[case::cached("pub fn handler(v: Cached<Query<Value>>) {}", "Query<Value>")]
fn local_paths_are_flagged(#[case] handler: &str, #[case] expected: &str) {
    let src = format!("pub struct Value {{ pub a: i32 }} {handler}");
    let message = run(&src).expect_err(handler).to_string();
    assert!(message.contains(&format!("`{expected}`")), "{message}");
}

#[test]
fn crate_path_into_another_route_module_is_not_local() {
    let src = "pub struct Value { pub a: i32 } \
               pub fn handler(Json(v): Json<crate::routes::other::Value>) {}";
    assert!(run(src).is_ok());
}
