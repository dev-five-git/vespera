//! How an `Option` field is written: serde writes `None` as `null` (the
//! response view requires the key, with a nullable schema) unless
//! `skip_serializing_if = "Option::is_none"` leaves it out — then the output
//! never holds `null`, and the response view documents the `Some` value as
//! an optional property. The request view accepts `null` either way.

use rstest::rstest;
use serde_json::{Value, json};

use super::*;
use crate::parser::schema::request_view::with_request_view;

/// `(property x, required)` of `struct S { <field> }` in one view.
fn view(field: &str, request: bool) -> (Value, Value) {
    let known = HashSet::from(["User".to_string()]);
    let defs = HashMap::from([("User".to_string(), "struct User { id: u8 }".to_string())]);
    let item: syn::ItemStruct = syn::parse_str(&format!("struct S {{ {field} }}")).unwrap();
    let build = || parse_struct_to_schema(&item, &known, &defs);
    let schema = serde_json::to_value(if request {
        with_request_view(build)
    } else {
        build()
    })
    .unwrap();
    (
        schema["properties"]["x"].clone(),
        schema["required"].clone(),
    )
}

const SKIP_NONE: &str = r#"#[serde(skip_serializing_if = "Option::is_none")]"#;

#[rstest]
#[case::plain("x: Option<String>", json!({"type": ["string", "null"]}), json!(["x"]))]
#[case::skip_none(
    &format!("{SKIP_NONE} x: Option<String>"),
    json!({"type": "string"}),
    Value::Null
)]
#[case::skip_none_std_path(
    r#"#[serde(skip_serializing_if = "std::option::Option::is_none")] x: Option<u8>"#,
    json!({"type": "integer", "format": "uint8", "minimum": 0, "maximum": 255}),
    Value::Null
)]
// `Some(None)` is still written as `null`.
#[case::skip_none_double_option(
    &format!("{SKIP_NONE} x: Option<Option<String>>"),
    json!({"type": ["string", "null"]}),
    Value::Null
)]
#[case::skip_none_ref(
    &format!("{SKIP_NONE} x: Option<User>"),
    json!({"$ref": "#/components/schemas/User"}),
    Value::Null
)]
#[case::skip_none_value_type(
    &format!("{SKIP_NONE} #[schema(value_type = Option<i64>)] x: Option<String>"),
    json!({"type": "integer", "format": "int64"}),
    Value::Null
)]
// A `value_type` that is no `Option` documents itself.
#[case::skip_none_plain_value_type(
    &format!("{SKIP_NONE} #[schema(value_type = i64)] x: Option<String>"),
    json!({"type": "integer", "format": "int64"}),
    Value::Null
)]
// Another predicate may still let a `None` through as `null`.
#[case::other_predicate(
    r#"#[serde(skip_serializing_if = "is_hidden")] x: Option<String>"#,
    json!({"type": ["string", "null"]}),
    Value::Null
)]
// Not an `Option`: the predicate only makes the key optional.
#[case::skip_none_non_option(
    &format!("{SKIP_NONE} x: String"),
    json!({"type": "string"}),
    Value::Null
)]
fn response_view(#[case] field: &str, #[case] schema: Value, #[case] required: Value) {
    assert_eq!(view(field, false), (schema, required), "{field}");
}

/// A tuple struct's skipped `Option` position is omitted by serde when `None`
/// (the later positions move left), so its response slot documents only the
/// `Some` value: no `null` in `prefixItems[1]`, which holds the position or
/// the `u32` that moves into it.
#[test]
fn tuple_skip_none_position_documents_some_only() {
    let item: syn::ItemStruct =
        syn::parse_str(&format!("struct P(u8, {SKIP_NONE} Option<u16>, u32);")).unwrap();
    let schema = serde_json::to_value(parse_struct_to_schema(
        &item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    ))
    .unwrap();
    let slot = &schema["prefixItems"][1];
    assert!(!slot.to_string().contains("null"), "{slot}");
}

/// The request view reads `null` for any `Option`, predicate or not.
#[rstest]
#[case::plain("x: Option<String>")]
#[case::skip_none(&format!("{SKIP_NONE} x: Option<String>"))]
fn request_view_stays_nullable(#[case] field: &str) {
    assert_eq!(
        view(field, true),
        (json!({"type": ["string", "null"]}), Value::Null),
        "{field}"
    );
}
