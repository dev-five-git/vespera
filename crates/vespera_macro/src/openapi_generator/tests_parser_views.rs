//! End-to-end checks of parser views: request parameters read through the
//! request (deserialize) view, transparent and enum-sourced extractors,
//! recursive generic definitions, a generic instance name colliding with a
//! user schema, and generic instances' `default = "fn"` values.

use super::tests_accuracy::{generate, pretty, route, schema, stored};
use crate::metadata::CollectedMetadata;

/// One GET route per `(path, fn_name, signature)` over `structs`.
fn doc_for(structs: &[(&str, &str)], routes: &[(&str, &str, &str)]) -> String {
    let mut metadata = CollectedMetadata::new();
    for (name, definition) in structs {
        metadata.structs.push(schema(name, definition));
    }
    let mut storage = Vec::new();
    for (path, fn_name, sig) in routes {
        metadata.routes.push(route(path, fn_name));
        storage.push(stored(fn_name, sig));
    }
    pretty(&generate(&metadata, &storage))
}

/// Path / query / header parameters are deserialized, so they document the
/// request view: a sided `rename_all` enum references its `Input`
/// component, and a nested struct is inlined from its request view.
#[test]
fn parameters_use_the_request_view() {
    let doc = doc_for(
        &[
            (
                "Sort",
                r#"#[serde(rename_all(serialize = "SCREAMING_SNAKE_CASE", deserialize = "lowercase"))]
                pub enum Sort { Asc, Desc }"#,
            ),
            (
                "Inner",
                r#"pub struct Inner { #[serde(rename(serialize = "Out", deserialize = "in"))] pub value: u8 }"#,
            ),
            (
                "ListQuery",
                "pub struct ListQuery { pub sort: Sort, pub inner: Inner }",
            ),
        ],
        &[
            (
                "/list",
                "list",
                "async fn list(Query(q): Query<ListQuery>) -> Json<Sort>",
            ),
            (
                "/by/{sort}",
                "by",
                "async fn by(Path(sort): Path<Sort>, x_sort: Header<Sort>) -> String",
            ),
        ],
    );
    insta::assert_snapshot!("parser_views_parameters_request_view", doc);
}

/// serde reads a `#[serde(transparent)]` struct as its single field, and a
/// `from = "E"` struct as `E`: `Query<Wrapped>` documents `Filter`'s
/// fields, `Path<UserId>` the `u64`, and `Query<Chosen>` (read as an enum)
/// one parameter of that enum.
#[test]
fn transparent_and_enum_sourced_extractors() {
    let doc = doc_for(
        &[
            (
                "Filter",
                "pub struct Filter { pub q: String, pub page: Option<u32> }",
            ),
            (
                "Wrapped",
                "#[serde(transparent)] pub struct Wrapped { #[serde(skip)] pub cache: u8, pub inner: Filter }",
            ),
            (
                "UserId",
                "#[serde(transparent)] pub struct UserId { pub id: u64 }",
            ),
            (
                "Kind",
                r#"#[serde(tag = "kind")] pub enum Kind { A { a: u32 }, B { b: String } }"#,
            ),
            (
                "Chosen",
                r#"#[serde(from = "Kind")] pub struct Chosen { pub raw: String }"#,
            ),
        ],
        &[
            (
                "/wrapped",
                "wrapped",
                "async fn wrapped(Query(w): Query<Wrapped>) -> String",
            ),
            (
                "/users/{user}",
                "user",
                "async fn user(Path(id): Path<UserId>) -> String",
            ),
            (
                "/chosen",
                "chosen",
                "async fn chosen(Query(c): Query<Chosen>, o: Option<Query<Chosen>>) -> String",
            ),
        ],
    );
    insta::assert_snapshot!("parser_views_transparent_extractors", doc);
}

/// A recursive generic definition refers back to its own component from
/// inside itself — an enum (`List<T>`) as well as a struct's request view
/// (`TreeInput`) — instead of materialising a spurious `List_T` /
/// `Tree_TInput` component.
#[test]
fn recursive_generic_definitions_refer_to_themselves() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(schema(
        "List",
        "pub enum List<T> { Nil, Cons(T, Box<List<T>>) }",
    ));
    metadata.structs.push(schema(
        "Tree",
        "pub struct Tree<T> { pub value: T, #[serde(default)] pub children: Vec<Tree<T>> }",
    ));
    metadata
        .routes
        .push(super::tests_accuracy::post("/list", "list"));
    metadata
        .routes
        .push(super::tests_accuracy::post("/tree", "tree"));
    let storage = [
        super::tests_accuracy::stored_post(
            "list",
            "async fn list(Json(l): Json<List<i32>>) -> Json<List<i32>>",
        ),
        super::tests_accuracy::stored_post(
            "tree",
            "async fn tree(Json(t): Json<Tree<i32>>) -> Json<Tree<i32>>",
        ),
    ];
    let doc = generate(&metadata, &storage);
    let components = doc.components.as_ref().and_then(|c| c.schemas.as_ref());
    insta::assert_snapshot!(
        "parser_views_recursive_generic_definitions",
        serde_json::to_string_pretty(&components).unwrap()
    );
}

/// A user component named like a recursive generic instance
/// (`#[schema(name = "Tree_i32")]` next to `Tree<i32>`) is a compile error:
/// every `$ref` to `Tree_i32` would resolve to it and the instance would be
/// dropped.
#[test]
fn generic_instance_name_colliding_with_a_user_schema_is_an_error() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(schema(
        "Tree",
        "pub struct Tree<T> { pub value: T, pub children: Vec<Tree<T>> }",
    ));
    metadata.structs.push(super::tests_accuracy::renamed(
        "Tree_i32",
        "X",
        r#"#[schema(name = "Tree_i32")] pub struct X { pub other: bool }"#,
    ));
    metadata
        .routes
        .push(super::tests_accuracy::route("/tree", "tree"));
    let storage = [stored("tree", "async fn tree() -> Json<Tree<i32>>")];
    let error = super::try_generate_openapi_doc_with_metadata(
        None, None, None, None, &metadata, None, &storage,
    )
    .expect_err("the instance `Tree_i32` collides with the user schema")
    .to_string();
    assert!(error.contains("`Tree_i32`"), "{error}");

    // Without the colliding schema the instance is a component of its own.
    metadata.structs.pop();
    let doc = generate(&metadata, &storage);
    let components = doc.components.and_then(|c| c.schemas).unwrap_or_default();
    assert!(components.contains_key("Tree_i32"));
}

/// A generic instance documents the `#[serde(default = "fn")]` value
/// `#[derive(Schema)]` resolved for the generic definition, like the generic
/// component itself: the recursive `Tree_i32` / `Tree_i32.Input` components
/// and the inlined `Page<i32>` of a response.
#[test]
fn generic_instances_document_default_fn_values() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(crate::metadata::StructMetadata {
        field_defaults: [("limit".to_string(), serde_json::json!(20))].into(),
        ..schema(
            "Tree",
            r#"pub struct Tree<T> { pub value: T, #[serde(default = "limit")] pub limit: u32, pub children: Vec<Tree<T>> }"#,
        )
    });
    metadata.structs.push(crate::metadata::StructMetadata {
        field_defaults: [("size".to_string(), serde_json::json!(50))].into(),
        ..schema(
            "Page",
            r#"pub struct Page<T> { pub items: Vec<T>, #[serde(default = "size")] pub size: u32 }"#,
        )
    });
    metadata
        .routes
        .push(super::tests_accuracy::post("/tree", "tree"));
    metadata.routes.push(route("/page", "page"));
    let storage = [
        super::tests_accuracy::stored_post(
            "tree",
            "async fn tree(Json(t): Json<Tree<i32>>) -> Json<Tree<i32>>",
        ),
        stored("page", "async fn page() -> Json<Page<i32>>"),
    ];
    let doc = generate(&metadata, &storage);
    let json = serde_json::to_value(&doc).unwrap();
    let schemas = &json["components"]["schemas"];
    for name in ["Tree", "Tree_i32", "Tree_i32.Input"] {
        assert_eq!(
            schemas[name]["properties"]["limit"]["default"], 20,
            "{name}: {schemas}"
        );
    }
    let page =
        &json["paths"]["/page"]["get"]["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(page["properties"]["size"]["default"], 50, "{page}");
}
