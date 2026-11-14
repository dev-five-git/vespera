//! End-to-end OpenAPI accuracy tests: renamed schemas, typed responses,
//! recursive generics, and route-level doc/deprecation metadata.

use vespera_core::{openapi::OpenApi, schema::SchemaRef};

use super::generate_openapi_doc_with_metadata;
use crate::{
    metadata::{CollectedMetadata, RouteMetadata, StructMetadata},
    route_impl::StoredRouteInfo,
};

/// A GET route at `path` whose handler signature is `sig`.
fn route(path: &str, fn_name: &str) -> RouteMetadata {
    RouteMetadata {
        method: "get".to_string(),
        path: path.to_string(),
        function_name: fn_name.to_string(),
        module_path: "routes".to_string(),
        file_path: format!("virtual/{fn_name}.rs"),
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

fn stored(fn_name: &str, sig: &str) -> StoredRouteInfo {
    StoredRouteInfo {
        fn_name: fn_name.to_string(),
        method: Some("get".to_string()),
        custom_path: None,
        success_status: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        file_path: None,
        fn_sig_str: sig.to_string(),
    }
}

fn schema(name: &str, definition: &str) -> StructMetadata {
    StructMetadata::new(name.to_string(), definition.to_string())
}

/// `schema` registered under a `#[schema(name = ...)]` that differs from the
/// Rust ident, as `#[derive(Schema)]` records it.
fn renamed(name: &str, ident: &str, definition: &str) -> StructMetadata {
    StructMetadata {
        explicit_schema_name: true,
        rust_ident: Some(ident.to_string()),
        ..schema(name, definition)
    }
}

fn generate(metadata: &CollectedMetadata, storage: &[StoredRouteInfo]) -> OpenApi {
    generate_openapi_doc_with_metadata(None, None, None, None, metadata, None, storage)
}

fn pretty(doc: &OpenApi) -> String {
    serde_json::to_string_pretty(doc).unwrap()
}

#[test]
fn renamed_schema_is_referenced_by_its_custom_name() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(renamed(
        "UserDto",
        "User",
        r#"#[derive(Schema)] #[schema(name = "UserDto")] pub struct User { pub id: i32 }"#,
    ));
    metadata.structs.push(renamed(
        "KindDto",
        "Kind",
        r#"#[schema(name = "KindDto")] pub enum Kind { A, B }"#,
    ));
    metadata.structs.push(schema(
        "Post",
        "pub struct Post { pub author: User, pub kind: Option<Kind> }",
    ));
    let mut get = route("/users/{id}", "get_user");
    get.typed_responses = Some(vec![
        (404, "User".to_string()),
        (409, "Elsewhere".to_string()),
    ]);
    metadata.routes.push(get);
    let storage = [stored(
        "get_user",
        "async fn get_user(Json(post): Json<Post>) -> Json<User>",
    )];

    let doc = generate(&metadata, &storage);

    insta::assert_snapshot!("accuracy_renamed_schema_refs", pretty(&doc));
}

#[test]
fn renamed_schema_alias_never_shadows_a_real_schema() {
    let mut metadata = CollectedMetadata::new();
    metadata
        .structs
        .push(schema("User", "pub struct User { pub real: bool }"));
    metadata.structs.push(renamed(
        "UserDto",
        "User",
        r#"#[schema(name = "UserDto")] pub struct User { pub id: i32 }"#,
    ));
    metadata.routes.push(route("/user", "user"));
    let doc = generate(
        &metadata,
        &[stored("user", "async fn user() -> Json<User>")],
    );

    let content = doc.paths["/user"].get.as_ref().unwrap().responses["200"]
        .content
        .as_ref()
        .unwrap();
    let Some(SchemaRef::Ref(reference)) = &content["application/json"].schema else {
        panic!("expected a $ref");
    };
    assert_eq!(reference.ref_path, "#/components/schemas/User");
}

/// A recursive generic becomes the named component `Tree_i32` (referenced
/// from inside itself); a non-recursive generic stays inline.
#[test]
fn recursive_generic_instantiation_becomes_a_component() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(schema(
        "Tree",
        "pub struct Tree<T> { pub value: T, pub children: Vec<Tree<T>> }",
    ));
    metadata.structs.push(schema(
        "Page",
        "pub struct Page<T> { pub items: Vec<T>, pub total: u64 }",
    ));
    metadata.routes.push(route("/tree", "tree"));
    metadata.routes.push(route("/page", "page"));
    let storage = [
        stored("tree", "async fn tree() -> Json<Tree<i32>>"),
        stored("page", "async fn page() -> Json<Page<Tree<i32>>>"),
    ];

    let doc = generate(&metadata, &storage);

    insta::assert_snapshot!("accuracy_recursive_generic_component", pretty(&doc));
}

fn post(path: &str, fn_name: &str) -> RouteMetadata {
    RouteMetadata {
        method: "post".to_string(),
        ..route(path, fn_name)
    }
}

fn stored_post(fn_name: &str, sig: &str) -> StoredRouteInfo {
    StoredRouteInfo {
        method: Some("post".to_string()),
        ..stored(fn_name, sig)
    }
}

/// Request bodies reference `{Name}Input` when serde reads a different shape
/// than it writes; responses keep `{Name}`. Identical views emit nothing.
#[test]
fn request_views_get_input_components() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(schema(
        "Settings",
        r#"pub struct Settings {
            #[serde(default)] pub theme: String,
            #[serde(skip_deserializing)] pub updated_at: String,
            #[serde(skip_serializing)] pub secret: String,
            #[serde(rename(serialize = "displayName", deserialize = "display_name"))] pub name: String,
            pub plain: bool,
        }"#,
    ));
    metadata.structs.push(schema(
        "Profile",
        "pub struct Profile { pub settings: Settings, pub tags: Vec<String> }",
    ));
    metadata
        .structs
        .push(schema("Page", "pub struct Page<T> { pub items: Vec<T> }"));
    metadata.structs.push(renamed(
        "AccountDto",
        "Account",
        r#"#[schema(name = "AccountDto")] #[serde(default)] pub struct Account { pub id: u32 }"#,
    ));
    // A real type already owns `AccountDtoInput`.
    metadata.structs.push(schema(
        "AccountDtoInput",
        "pub struct AccountDtoInput { pub x: u8 }",
    ));
    metadata
        .structs
        .push(schema("Plain", "pub struct Plain { pub x: u8 }"));
    for (path, name) in [
        ("/profile", "save"),
        ("/page", "page"),
        ("/account", "account"),
    ] {
        metadata.routes.push(post(path, name));
    }
    metadata.routes.push(route("/plain", "plain"));
    let storage = [
        stored_post(
            "save",
            "async fn save(Json(p): Json<Profile>) -> Json<Profile>",
        ),
        stored_post(
            "page",
            "async fn page(Form(p): Form<Page<Settings>>) -> Json<Page<Settings>>",
        ),
        stored_post(
            "account",
            "async fn account(Json(a): Json<Account>) -> Json<Account>",
        ),
        stored(
            "plain",
            "async fn plain(Json(p): Json<Plain>) -> Json<Plain>",
        ),
    ];

    let doc = generate(&metadata, &storage);

    insta::assert_snapshot!("accuracy_request_view_inputs", pretty(&doc));
}

#[test]
fn input_names_skip_unparsable_definitions() {
    let mut metadata = CollectedMetadata::new();
    metadata
        .structs
        .push(schema("Broken", "pub struct Broken { serde"));
    metadata
        .structs
        .push(schema("A", "pub struct A { #[serde(default)] pub x: u8 }"));
    let known = std::collections::HashSet::from(["A", "Broken"]);
    let names = super::component_schemas::input_names(&metadata, &known);
    assert_eq!(names.get("A").map(String::as_str), Some("AInput"));
    assert_eq!(names.len(), 1);
}

/// Inputs no request body reaches emit no `Input` component (`Out`). A
/// field-level `default` makes the request view differ: serde always writes
/// the field, so the response keeps it required while `OptInput` does not
/// (and `Holder` follows transitively).
#[test]
fn identical_or_unused_request_views_emit_nothing() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(schema(
        "Opt",
        "pub struct Opt { #[serde(default)] pub x: u8 }",
    ));
    metadata.structs.push(schema(
        "Holder",
        "pub struct Holder { pub opt: Opt, pub same: Same }",
    ));
    // `default` on an `Option` field: optional in both views, so the built
    // `SameInput` equals `Same` and is dropped.
    metadata.structs.push(schema(
        "Same",
        "pub struct Same { #[serde(default)] pub x: Option<u8> }",
    ));
    metadata.structs.push(schema(
        "Out",
        "pub struct Out { #[serde(skip_deserializing)] pub id: u8 }",
    ));
    metadata.routes.push(post("/holder", "holder"));
    let storage = [stored_post(
        "holder",
        "async fn holder(Json(h): Json<Holder>) -> Json<Out>",
    )];

    let doc = generate(&metadata, &storage);

    let names: Vec<&String> = doc
        .components
        .as_ref()
        .and_then(|c| c.schemas.as_ref())
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        names,
        ["Holder", "HolderInput", "Opt", "OptInput", "Out", "Same"]
    );
}
