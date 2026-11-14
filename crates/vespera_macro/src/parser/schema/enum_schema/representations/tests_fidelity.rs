//! Enum schemas accept exactly what serde writes (response view) and reads
//! (request view): variant aliases, `#[serde(other)]`, unit-struct newtypes,
//! codecs on variants and tuple-variant fields, one-sided skips. Each
//! snapshot holds both views.

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use crate::parser::schema::{enum_schema::parse_enum_to_schema, request_view::with_request_view};

/// `response:` / `request:` JSON of `src`.
fn both_views(src: &str) -> String {
    let item: syn::ItemEnum = syn::parse_str(src).unwrap();
    let known = HashSet::from(["Ping".to_string(), "Payload".to_string()]);
    let definitions = HashMap::from([
        ("Ping".to_string(), "struct Ping;".to_string()),
        (
            "Payload".to_string(),
            "struct Payload { id: u32 }".to_string(),
        ),
    ]);
    let response = parse_enum_to_schema(&item, &known, &definitions);
    let request = with_request_view(|| parse_enum_to_schema(&item, &known, &definitions));
    format!(
        "{src}\nresponse: {}\nrequest: {}",
        serde_json::to_string(&response).unwrap(),
        serde_json::to_string(&request).unwrap()
    )
}

#[rstest]
// M1: variant aliases are accepted tags of the request view.
#[case::alias_unit(
    "alias_unit",
    r#"enum Color { #[serde(alias = "crimson")] Red, Blue }"#
)]
#[case::alias_external(
    "alias_external",
    r#"enum Shape { #[serde(alias = "sq")] Square(u8), #[serde(alias = "pt")] Point }"#
)]
#[case::alias_internal(
    "alias_internal",
    r#"#[serde(tag = "t")] enum Shape { #[serde(alias = "c")] Circle { r: u8 }, Dot }"#
)]
#[case::alias_adjacent(
    "alias_adjacent",
    r#"#[serde(tag = "t", content = "c")] enum Shape { #[serde(alias = "c")] Circle(u8) }"#
)]
#[case::alias_field_in_variant(
    "alias_field_in_variant",
    r#"enum Shape { Circle { #[serde(alias = "radius")] r: u8 } }"#
)]
// M2: `#[serde(other)]` reads any unknown tag and writes its own name.
#[case::other_unit("other_unit", "enum Color { Red, Blue, #[serde(other)] Unknown }")]
#[case::other_external(
    "other_external",
    "enum Shape { Square(u8), Point, #[serde(other)] Unknown }"
)]
#[case::other_internal(
    "other_internal",
    r#"#[serde(tag = "t")] enum Shape { Circle { r: u8 }, #[serde(other)] Unknown }"#
)]
// M5: an internally tagged newtype over a unit struct is the tag alone.
#[case::internal_unit_struct(
    "internal_unit_struct",
    r#"#[serde(tag = "t")] enum Msg { Ping(Ping), Boxed(Box<Ping>), Data(Payload) }"#
)]
// M6: codecs and `value_type` per side on variants and tuple-variant fields.
#[case::tuple_variant_fields(
    "tuple_variant_fields",
    r#"enum E { Pair(#[schema(value_type = String)] Id, #[serde(deserialize_with = "d")] u8), One(#[serde(with = "m")] Thing) }"#
)]
#[case::variant_codec_external(
    "variant_codec_external",
    r#"enum E { #[serde(with = "m")] Wrapped(Thing), #[serde(serialize_with = "s")] Named { a: u8 }, Plain }"#
)]
#[case::variant_codec_internal(
    "variant_codec_internal",
    r#"#[serde(tag = "t")] enum E { #[serde(with = "m")] Named { a: u8 }, #[serde(with = "m")] Newtype(Payload) }"#
)]
#[case::variant_codec_adjacent(
    "variant_codec_adjacent",
    r#"#[serde(tag = "t", content = "c")] enum E { #[serde(deserialize_with = "d")] Newtype(u8) }"#
)]
#[case::variant_codec_untagged(
    "variant_codec_untagged",
    r#"#[serde(untagged)] enum E { #[serde(with = "m")] Newtype(Thing), Pair(u8, #[serde(with = "m")] Thing) }"#
)]
// L1: a tuple variant's length honours one-sided skips.
#[case::tuple_variant_skips(
    "tuple_variant_skips",
    "enum E { Triple(u8, #[serde(skip_serializing)] u16, #[serde(skip_deserializing)] u32) }"
)]
// M3: `rename` / `rename_all` values are not the `tag` key.
#[case::rename_is_not_tag(
    "rename_is_not_tag",
    r#"#[serde(rename = "Stage", rename_all = "camelCase")] enum Stage { InProgress(u8), Done }"#
)]
fn enum_views(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(format!("enum_views_{name}"), both_views(src));
}
