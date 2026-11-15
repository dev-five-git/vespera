//! Only paths that name the route module's own items are matched against its
//! local types (`serde_json::Value` is not a local `struct Value`), and
//! `Path<T>` / `OptionalQuery<T>` / `WithRejection` / `Cached` extractors are
//! checked like `Json<T>`.

use rstest::rstest;

use super::*;
use crate::metadata::{CollectedMetadata, RouteMetadata};

fn run(src: &str) -> syn::Result<()> {
    run_with_test_only(src, &[])
}

/// `run` with `test_only` `(file_path, function_name)` entries in
/// `CollectedMetadata::test_only_routes`.
fn run_with_test_only(src: &str, test_only: &[(&str, &str)]) -> syn::Result<()> {
    let mut metadata = CollectedMetadata::new();
    metadata.test_only_routes = test_only
        .iter()
        .map(
            |(file_path, function_name)| crate::metadata::TestOnlyRoute {
                file_path: (*file_path).to_string(),
                function_name: (*function_name).to_string(),
            },
        )
        .collect();
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
#[case::foreign_tuple("pub fn handler(Path((a, b)): Path<(serde_json::Value, u32)>) {}")]
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
// Tuples, arrays, slices (and parenthesised types) are inspected too.
#[case::path_tuple("pub fn handler(Path((a, b)): Path<(Value, u32)>) {}", "Path<Value>")]
#[case::array("pub fn handler(Json(v): Json<[Value; 2]>) {}", "Json<Value>")]
#[case::boxed_slice("pub fn handler(Json(v): Json<Box<[Value]>>) {}", "Json<Value>")]
#[case::paren("pub fn handler(Json(v): Json<(Value)>) {}", "Json<Value>")]
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

/// A route file collected through the `#[route]` fast path has no AST in
/// the collector's cache: it is parsed (memoized) and checked all the same,
/// next to files the cache does hold; an unreadable path is skipped.
#[test]
fn fast_path_route_files_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("users.rs");
    std::fs::write(
        &file,
        "pub struct Local { pub a: i32 }\n\
         pub fn create(Json(v): Json<Local>) {}\n\
         pub fn list() {}",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let route = |function_name: &str, file_path: &str| RouteMetadata {
        method: "get".to_string(),
        path: "/x".to_string(),
        function_name: function_name.to_string(),
        module_path: "routes::users".to_string(),
        file_path: file_path.to_string(),
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
    };
    let real = file.to_string_lossy().into_owned();
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(route("handler", "x.rs"));
    metadata.routes.push(route("gone", "missing/nowhere.rs"));
    metadata.routes.push(route("list", &real));
    metadata.routes.push(route("create", &real));
    let cached: syn::File = syn::parse_str("pub fn handler() {}").unwrap();
    let file_cache = HashMap::from([("x.rs".to_string(), cached)]);

    let message = validate_schema_backed_extractors_with_cache(&metadata, &file_cache)
        .expect_err("the fast-path file declares a non-`Schema` `Local`")
        .to_string();
    assert!(message.contains("`Json<Local>`"), "{message}");

    // Nothing to parse: the cached ASTs alone are checked.
    metadata.routes.truncate(1);
    assert!(validate_schema_backed_extractors_with_cache(&metadata, &file_cache).is_ok());
}

/// A `cfg(test)`-only route is mounted but never documented, so its
/// non-`Schema` extractor type is no error (`cargo test` passes where
/// `cargo build` does). A test-only entry for another file or function does
/// not exempt the documented route.
#[rstest]
#[case::documented(&[], false)]
#[case::test_only(&[("x.rs", "handler")], true)]
#[case::other_file(&[("y.rs", "handler")], false)]
#[case::other_fn(&[("x.rs", "other")], false)]
fn test_only_routes_are_not_checked(#[case] test_only: &[(&str, &str)], #[case] ok: bool) {
    let src = "pub struct Probe { pub a: i32 } pub fn handler(Json(p): Json<Probe>) {}";
    assert_eq!(run_with_test_only(src, test_only).is_ok(), ok);
}
