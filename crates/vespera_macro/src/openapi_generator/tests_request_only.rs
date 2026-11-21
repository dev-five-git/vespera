//! Request-only types keep their component name: a type whose request view
//! differs gets a separate `{Name}Input` only when its response view is used
//! too (see `component_schemas::collapse_request_only_inputs`).

use serde_json::{Value, json};

use super::tests_accuracy::{generate, post, renamed, schema, stored_post};
use crate::{
    metadata::CollectedMetadata, openapi_generator::try_generate_openapi_doc_with_metadata,
};

/// The document for `structs` and one POST route per `(path, fn, sig)`, as
/// JSON.
fn doc_for(structs: &[(&str, &str)], routes: &[(&str, &str, &str)]) -> Value {
    let mut metadata = CollectedMetadata::new();
    for (name, definition) in structs {
        metadata.structs.push(schema(name, definition));
    }
    let mut storage = Vec::new();
    for (path, fn_name, sig) in routes {
        metadata.routes.push(post(path, fn_name));
        storage.push(stored_post(fn_name, sig));
    }
    serde_json::to_value(generate(&metadata, &storage)).unwrap()
}

fn component_names(doc: &Value) -> Vec<&str> {
    doc["components"]["schemas"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn request_ref<'a>(doc: &'a Value, path: &str) -> &'a Value {
    &doc["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"]
}

fn schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

const PORTFOLIO_UPDATE: &str = r#"#[serde(rename_all = "camelCase")]
pub struct PortfolioUpdateRequest {
    #[serde(default)]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    #[schema(value_type = Option<NaiveDate>)]
    pub production_end_date: Option<Option<NaiveDate>>,
}"#;

/// A request-only type whose views differ is emitted once, as its request
/// view, under its own name: no `PortfolioUpdateRequestInput`, no orphan
/// response view. The `deserialize_with` field is documented by its
/// `value_type` (`Option<NaiveDate>` → a nullable date, not required; its
/// `#[serde(default)]` documents `default: null`): a custom deserializer with
/// no well-known wire shape needs one.
#[test]
fn request_only_type_keeps_its_name() {
    let doc = doc_for(
        &[
            ("PortfolioUpdateRequest", PORTFOLIO_UPDATE),
            ("Portfolio", "pub struct Portfolio { pub id: u64 }"),
        ],
        &[(
            "/portfolio",
            "update",
            "async fn update(Json(r): Json<PortfolioUpdateRequest>) -> Json<Portfolio>",
        )],
    );
    assert_eq!(
        component_names(&doc),
        ["Portfolio", "PortfolioUpdateRequest"]
    );
    assert_eq!(
        *request_ref(&doc, "/portfolio"),
        schema_ref("PortfolioUpdateRequest")
    );
    let request = &doc["components"]["schemas"]["PortfolioUpdateRequest"];
    assert_eq!(
        request["properties"]["productionEndDate"],
        json!({"type": ["string", "null"], "format": "date", "default": null})
    );
    insta::assert_snapshot!(
        "request_only_portfolio_update_request",
        serde_json::to_string_pretty(request).unwrap()
    );
}

/// An `Option` field of a request-only type is optional on input: serde reads
/// a missing `Option` key as `None`, so `note` is not in `required`.
#[test]
fn request_only_option_field_is_not_required() {
    let doc = doc_for(
        &[(
            "CreateItem",
            "pub struct CreateItem { pub name: String, pub note: Option<String> }",
        )],
        &[(
            "/items",
            "create",
            "async fn create(Json(i): Json<CreateItem>) -> String",
        )],
    );
    assert_eq!(component_names(&doc), ["CreateItem"]);
    assert_eq!(*request_ref(&doc, "/items"), schema_ref("CreateItem"));
    assert_eq!(
        doc["components"]["schemas"]["CreateItem"]["required"],
        json!(["name"])
    );
}

/// A type only returned keeps its single response view (`x` required).
#[test]
fn response_only_type_is_unchanged() {
    let doc = doc_for(
        &[(
            "Settings",
            "pub struct Settings { #[serde(default)] pub x: u8 }",
        )],
        &[("/settings", "get", "async fn get() -> Json<Settings>")],
    );
    assert_eq!(component_names(&doc), ["Settings"]);
    assert_eq!(
        doc["components"]["schemas"]["Settings"]["required"],
        json!(["x"])
    );
}

/// A type read and returned keeps both views: `{Name}` and `{Name}Input`.
#[test]
fn type_used_on_both_sides_gets_an_input() {
    let doc = doc_for(
        &[(
            "Settings",
            "pub struct Settings { #[serde(default)] pub x: u8 }",
        )],
        &[(
            "/settings",
            "put",
            "async fn put(Json(s): Json<Settings>) -> Json<Settings>",
        )],
    );
    assert_eq!(component_names(&doc), ["Settings", "SettingsInput"]);
    assert_eq!(*request_ref(&doc, "/settings"), schema_ref("SettingsInput"));
    assert_eq!(
        doc["components"]["schemas"]["Settings"]["required"],
        json!(["x"])
    );
    assert_eq!(
        doc["components"]["schemas"]["SettingsInput"]["required"],
        Value::Null
    );
}

const CREATE_ORDER: (&str, &str) = (
    "CreateOrder",
    "pub struct CreateOrder { pub items: Vec<LineItem> }",
);
const LINE_ITEM: (&str, &str) = (
    "LineItem",
    "pub struct LineItem { pub sku: String, #[serde(default)] pub qty: u32 }",
);
const ORDER: (&str, &str) = ("Order", "pub struct Order { pub id: u64 }");

fn items_ref(doc: &Value, component: &str) -> Value {
    doc["components"]["schemas"][component]["properties"]["items"]["items"].clone()
}

/// `CreateOrder { items: Vec<LineItem> }` read only, `LineItem` nowhere
/// else: both keep their names, as their request views.
#[test]
fn transitive_request_only_types_keep_their_names() {
    let doc = doc_for(
        &[CREATE_ORDER, LINE_ITEM, ORDER],
        &[(
            "/orders",
            "create",
            "async fn create(Json(o): Json<CreateOrder>) -> Json<Order>",
        )],
    );
    assert_eq!(component_names(&doc), ["CreateOrder", "LineItem", "Order"]);
    assert_eq!(*request_ref(&doc, "/orders"), schema_ref("CreateOrder"));
    assert_eq!(items_ref(&doc, "CreateOrder"), schema_ref("LineItem"));
    assert_eq!(
        doc["components"]["schemas"]["LineItem"]["required"],
        json!(["sku"])
    );
}

/// `LineItem` is also returned: it keeps `LineItem` + `LineItemInput`, and
/// the request-only `CreateOrder` (its request view) references the input.
#[test]
fn transitive_type_also_returned_keeps_its_input() {
    let doc = doc_for(
        &[CREATE_ORDER, LINE_ITEM, ORDER],
        &[
            (
                "/orders",
                "create",
                "async fn create(Json(o): Json<CreateOrder>) -> Json<Order>",
            ),
            ("/items", "items", "async fn items() -> Json<Vec<LineItem>>"),
        ],
    );
    assert_eq!(
        component_names(&doc),
        ["CreateOrder", "LineItem", "LineItemInput", "Order"]
    );
    assert_eq!(*request_ref(&doc, "/orders"), schema_ref("CreateOrder"));
    assert_eq!(items_ref(&doc, "CreateOrder"), schema_ref("LineItemInput"));
    assert_eq!(
        doc["components"]["schemas"]["LineItem"]["required"],
        json!(["sku", "qty"])
    );
}

/// `CreateOrder` is returned too: its response view reaches `LineItem`, so
/// both types keep both views.
#[test]
fn transitive_types_used_on_both_sides_keep_their_inputs() {
    let doc = doc_for(
        &[CREATE_ORDER, LINE_ITEM],
        &[(
            "/orders",
            "create",
            "async fn create(Json(o): Json<CreateOrder>) -> Json<CreateOrder>",
        )],
    );
    assert_eq!(
        component_names(&doc),
        [
            "CreateOrder",
            "CreateOrderInput",
            "LineItem",
            "LineItemInput"
        ]
    );
    assert_eq!(
        *request_ref(&doc, "/orders"),
        schema_ref("CreateOrderInput")
    );
    assert_eq!(items_ref(&doc, "CreateOrder"), schema_ref("LineItem"));
    assert_eq!(
        items_ref(&doc, "CreateOrderInput"),
        schema_ref("LineItemInput")
    );
}

/// A documented type no route uses stays documented with what it references:
/// `Wrapper` (unused) keeps `Inner`'s response view alive, so the
/// request-only use of `Inner` still goes through `InnerInput`.
#[test]
fn unused_types_keep_the_response_views_they_reference() {
    let doc = doc_for(
        &[
            ("Wrapper", "pub struct Wrapper { pub inner: Inner }"),
            ("Inner", "pub struct Inner { #[serde(default)] pub x: u8 }"),
        ],
        &[(
            "/inner",
            "save",
            "async fn save(Json(i): Json<Inner>) -> String",
        )],
    );
    assert_eq!(component_names(&doc), ["Inner", "InnerInput", "Wrapper"]);
    assert_eq!(*request_ref(&doc, "/inner"), schema_ref("InnerInput"));
}

const USER: (&str, &str) = ("User", "pub struct User { #[serde(default)] pub x: u8 }");
const USER_INPUT: (&str, &str) = ("UserInput", "pub struct UserInput { pub y: u8 }");

/// A real `UserInput` type owns that name: a `User` read and returned gets
/// `UserInput2`; read only, it keeps `User` and the real `UserInput` is
/// left alone.
#[test]
fn input_name_collisions() {
    let both = doc_for(
        &[USER, USER_INPUT],
        &[(
            "/user",
            "save",
            "async fn save(Json(u): Json<User>) -> Json<User>",
        )],
    );
    assert_eq!(component_names(&both), ["User", "UserInput", "UserInput2"]);
    assert_eq!(*request_ref(&both, "/user"), schema_ref("UserInput2"));

    let request_only = doc_for(
        &[USER, USER_INPUT],
        &[(
            "/user",
            "save",
            "async fn save(Json(u): Json<User>) -> Json<UserInput>",
        )],
    );
    assert_eq!(component_names(&request_only), ["User", "UserInput"]);
    assert_eq!(*request_ref(&request_only, "/user"), schema_ref("User"));
    let schemas = &request_only["components"]["schemas"];
    assert_eq!(schemas["User"]["required"], Value::Null);
    assert_eq!(schemas["UserInput"]["required"], json!(["y"]));
}

/// A recursive generic instance read only: `Tree_i32.Input` becomes
/// `Tree_i32` (its request view), and its self-references follow.
#[test]
fn request_only_generic_instance_keeps_its_name() {
    let doc = doc_for(
        &[(
            "Tree",
            "pub struct Tree<T> { pub value: T, #[serde(default)] pub children: Vec<Tree<T>> }",
        )],
        &[(
            "/tree",
            "tree",
            "async fn tree(Json(t): Json<Tree<i32>>) -> String",
        )],
    );
    let names = component_names(&doc);
    assert!(
        !names.iter().any(|name| name.contains("Input")),
        "{names:?}"
    );
    let text = serde_json::to_string(&doc).unwrap();
    assert!(!text.contains(".Input"), "{text}");
    assert_eq!(
        doc["components"]["schemas"]["Tree_i32"]["required"],
        json!(["value"])
    );
}

/// A field typed by a user `type` alias is not resolved anywhere (no alias
/// registry reaches the schema or the view code), so `ttl: Ttl` documents as
/// the unresolved any-value `{}` in both views. The needle pre-filter and
/// `request_view::type_view_differs` are alias-blind alike, so no `TimedInput`
/// is built and the request body is `Timed` itself. The request schema is
/// consistent with the documented `{}`; it is less precise than the direct
/// `std::time::Duration` field, not wrong.
#[test]
fn alias_field_builds_no_request_view_like_its_unresolved_type() {
    let doc = doc_for(
        &[("Timed", "pub struct Timed { pub ttl: Ttl }")],
        &[(
            "/t",
            "post_t",
            "async fn post_t(Json(body): Json<Timed>) -> String",
        )],
    );
    let names = component_names(&doc);
    assert!(!names.contains(&"TimedInput"), "{names:?}");
    assert_eq!(
        doc["components"]["schemas"]["Timed"]["properties"]["ttl"],
        json!({})
    );
    assert_eq!(request_ref(&doc, "/t"), &schema_ref("Timed"));
}

/// A user component named `Pet.Input` (`#[schema(name = ...)]`) is rejected
/// with a spanned error, on a response-only type (where the bare `$ref` would
/// dangle once the component is pruned) and on a request-only one (where it
/// would be silently renamed to `Pet`): round-39 MR2-M1.
#[rstest::rstest]
#[case::response_only("get_pet", "async fn get_pet() -> Json<Pet>")]
#[case::request_only("create_pet", "async fn create_pet(Json(pet): Json<Pet>) -> String")]
fn explicit_input_schema_name_is_rejected(#[case] fn_name: &str, #[case] sig: &str) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(renamed(
        "Pet.Input",
        "Pet",
        "pub struct Pet { pub name: String }",
    ));
    metadata.routes.push(post("/pet", fn_name));
    let storage = vec![stored_post(fn_name, sig)];
    let err =
        try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &storage)
            .unwrap_err();
    assert!(
        err.to_string().contains("`Pet.Input` ends in `.Input`"),
        "{err}"
    );
}
