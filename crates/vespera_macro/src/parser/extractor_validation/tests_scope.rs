//! Only paths that name the route module's own items are matched against its
//! local types (`serde_json::Value` is not a local `struct Value`), and
//! `Path<T>` / `OptionalQuery<T>` / `WithRejection` / `Cached` extractors are
//! checked like `Json<T>`.

use rstest::rstest;

use super::*;
use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};

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
                // Legacy entry without a method: matches by name alone.
                method: String::new(),
            },
        )
        .collect();
    metadata.routes.push(handler_route("get"));
    check_with(&metadata, src)
}

/// The `handler` route of `x.rs` with `method`.
fn handler_route(method: &str) -> RouteMetadata {
    RouteMetadata {
        method: method.to_string(),
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
    }
}

/// [`check_extractors`] over `src` as the `x.rs` file of `metadata`.
fn check_with(metadata: &CollectedMetadata, src: &str) -> syn::Result<()> {
    let ast: syn::File = syn::parse_str(src).expect("source parses");
    let file_cache = HashMap::from([("x.rs".to_string(), ast)]);
    check_extractors(metadata, &file_cache)
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

/// Two-twin layout: a production `#[route(post)] fn handler` next to a
/// `#[cfg(test)] #[route(get)] fn handler`. The `cfg(test)` twin exempts the
/// POST route only when it is the same method (`documented_view` matches the
/// method too); a non-`Schema` extractor on the POST route must still fail.
#[rstest]
#[case::same_method_twin_exempts("post", true)]
#[case::other_method_twin_does_not("get", false)]
fn test_only_twin_must_match_the_route_method(#[case] twin_method: &str, #[case] ok: bool) {
    let mut metadata = CollectedMetadata::new();
    metadata.test_only_routes = vec![crate::metadata::TestOnlyRoute {
        file_path: "x.rs".to_string(),
        function_name: "handler".to_string(),
        method: twin_method.to_string(),
    }];
    metadata.routes.push(handler_route("post"));
    let src = "pub struct Probe { pub a: i32 } pub fn handler(Json(p): Json<Probe>) {}";
    assert_eq!(check_with(&metadata, src).is_ok(), ok);
}

/// The handler of a route is the same-named fn of its own method that is not
/// test-only, whatever order the twins come in. Here the `cfg(test)` GET twin
/// comes first and takes a non-`Schema` `Query<Fixture>`; the production POST
/// fn takes a `Schema` `Json<Body>` and must be checked against its own
/// signature (the first-match lookup checked the twin and rejected valid code).
#[test]
fn production_twin_is_checked_not_the_cfg_test_twin() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(handler_route("post"));
    metadata.structs.push(crate::metadata::StructMetadata::new(
        "Body".to_string(),
        String::new(),
    ));
    let src = "pub struct Fixture { pub a: i32 } pub struct Body { pub a: i32 } \
               #[cfg(test)] #[route(get)] pub fn handler(Query(q): Query<Fixture>) {} \
               #[route(post)] pub fn handler(Json(b): Json<Body>) {}";
    assert!(check_with(&metadata, src).is_ok());
}

/// The mirror case: the `cfg(test)` GET twin with a `Schema` extractor comes
/// first, and the production POST fn with a non-`Schema` `Json<Local>` must
/// still be flagged.
#[test]
fn production_twin_non_schema_extractor_is_flagged() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(handler_route("post"));
    metadata.structs.push(crate::metadata::StructMetadata::new(
        "Body".to_string(),
        String::new(),
    ));
    let src = "pub struct Local { pub a: i32 } pub struct Body { pub a: i32 } \
               #[cfg(test)] #[route(get)] pub fn handler(Json(b): Json<Body>) {} \
               #[route(post)] pub fn handler(Json(l): Json<Local>) {}";
    let message = check_with(&metadata, src)
        .expect_err("POST handler")
        .to_string();
    assert!(message.contains("`Json<Local>`"), "{message}");
}

/// A bare `#[route]` is a GET, `#[vespera::route(..)]` counts too, the
/// name-value form (`#[route = "post"]`) names its method like the list form,
/// and a fn routed to another method is not this route's handler.
#[rstest]
#[case::bare_is_get("#[route] pub fn handler(Json(l): Json<Local>) {}", "get", true)]
#[case::qualified_post(
    "#[vespera::route(post)] pub fn handler(Json(l): Json<Local>) {}",
    "post",
    true
)]
#[case::name_value_post(
    "#[route = \"post\"] pub fn handler(Json(l): Json<Local>) {}",
    "post",
    true
)]
#[case::name_value_upper_post(
    "#[route = \"POST\"] pub fn handler(Json(l): Json<Local>) {}",
    "post",
    true
)]
#[case::name_value_other_method_ignored(
    "#[route = \"put\"] pub fn handler(Json(l): Json<Local>) {}",
    "get",
    false
)]
#[case::other_method_ignored("#[route(put)] pub fn handler(Json(l): Json<Local>) {}", "get", false)]
fn route_attribute_spellings_pick_the_handler(
    #[case] handler: &str,
    #[case] method: &str,
    #[case] checked: bool,
) {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(handler_route(method));
    let src = format!("pub struct Local {{ pub a: i32 }} {handler}");
    assert_eq!(check_with(&metadata, &src).is_err(), checked, "{handler}");
}

/// Two route files: the handler `x.rs` (`routes::x`) and the sibling `other.rs`
/// (`routes::other`) with `sibling_src`; `known` are the `Schema` type names.
fn check_with_sibling(handler_src: &str, sibling_src: &str, known: &[&str]) -> syn::Result<()> {
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(handler_route("get"));
    let mut sibling = handler_route("get");
    sibling.function_name = "sibling".to_string();
    sibling.module_path = "routes::other".to_string();
    sibling.file_path = "other.rs".to_string();
    metadata.routes.push(sibling);
    for name in known {
        metadata
            .structs
            .push(StructMetadata::new((*name).to_string(), String::new()));
    }
    let file_cache = HashMap::from([
        (
            "x.rs".to_string(),
            syn::parse_str(handler_src).expect("handler parses"),
        ),
        (
            "other.rs".to_string(),
            syn::parse_str(sibling_src).expect("sibling parses"),
        ),
    ]);
    check_extractors(&metadata, &file_cache)
}

/// A qualified path (`crate::` / `super::`) into another route file names its
/// type inline, with no `use`: a non-`Schema` struct there is flagged, however
/// the path is nested in the extractor's type.
#[rstest]
#[case::crate_path("pub fn handler(Json(b): Json<crate::routes::other::Bar>) {}")]
#[case::super_path("pub fn handler(Json(b): Json<super::other::Bar>) {}")]
#[case::nested_containers(
    "pub fn handler(Json(b): Json<Vec<Option<crate::routes::other::Bar>>>) {}"
)]
#[case::reference("pub fn handler(Json(b): Json<&crate::routes::other::Bar>) {}")]
#[case::array("pub fn handler(Json(b): Json<[super::other::Bar; 2]>) {}")]
#[case::slice("pub fn handler(Json(b): Json<Box<[super::other::Bar]>>) {}")]
#[case::paren("pub fn handler(Json(b): Json<(super::other::Bar)>) {}")]
#[case::tuple("pub fn handler(Path((a, b)): Path<(u8, crate::routes::other::Bar)>) {}")]
#[case::path_extractor("pub fn handler(Path(b): Path<crate::routes::other::Bar>) {}")]
#[case::optional_extractor("pub fn handler(v: Option<Query<crate::routes::other::Bar>>) {}")]
fn inline_qualified_route_types_are_flagged(#[case] handler: &str) {
    let sibling = "pub struct Bar { pub a: i32 }";
    let message = check_with_sibling(handler, sibling, &[])
        .expect_err(handler)
        .to_string();
    assert!(message.contains("`Bar`"), "{message}");
}

/// Qualified paths that do not name a non-`Schema` type of a route file are
/// left alone: a `Schema` type, another module, an external crate, a leading
/// `::` (an external crate), a `self` path into a child module that is no route
/// file, a `super` chain that lands outside the route folder, and a `super`
/// chain that climbs above the crate root.
#[rstest]
#[case::schema_declared("pub fn handler(Json(b): Json<crate::routes::other::Bar>) {}", &["Bar"])]
#[case::other_module("pub fn handler(Json(b): Json<crate::models::Bar>) {}", &[])]
#[case::external_crate("pub fn handler(Json(b): Json<serde_json::Bar>) {}", &[])]
#[case::leading_colon("pub fn handler(Json(b): Json<::routes::other::Bar>) {}", &[])]
#[case::self_path_into_a_child_module("pub fn handler(Json(b): Json<self::other::Bar>) {}", &[])]
#[case::super_chain_leaves_routes("pub fn handler(Json(b): Json<super::super::other::Bar>) {}", &[])]
#[case::super_chain_above_root("pub fn handler(Json(b): Json<super::super::super::other::Bar>) {}", &[])]
fn inline_qualified_paths_outside_non_schema_route_types_are_ok(
    #[case] handler: &str,
    #[case] known: &[&str],
) {
    let sibling = "pub struct Bar { pub a: i32 }";
    assert!(
        check_with_sibling(handler, sibling, known).is_ok(),
        "{handler}"
    );
}
