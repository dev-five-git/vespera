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
    let known = HashSet::from([
        "Ping".to_string(),
        "Payload".to_string(),
        "Closed".to_string(),
        "Shape".to_string(),
        "Kind".to_string(),
        "Mode".to_string(),
        "Open".to_string(),
        "OpenUnit".to_string(),
    ]);
    let definitions = HashMap::from([
        ("Ping".to_string(), "struct Ping;".to_string()),
        (
            "Closed".to_string(),
            "#[serde(deny_unknown_fields)] struct Closed { id: u32 }".to_string(),
        ),
        (
            "Payload".to_string(),
            "struct Payload { id: u32 }".to_string(),
        ),
        (
            "Shape".to_string(),
            "enum Shape { Square(u8), Circle { r: u8 } }".to_string(),
        ),
        (
            "Kind".to_string(),
            "enum Kind { Point, #[serde(alias = \"pt\")] Dot, Square(u8) }".to_string(),
        ),
        ("Mode".to_string(), "enum Mode { On, Off }".to_string()),
        (
            "Open".to_string(),
            "enum Open { Square(u8), #[serde(other)] Unknown }".to_string(),
        ),
        (
            "OpenUnit".to_string(),
            "enum OpenUnit { On, #[serde(other)] Unknown }".to_string(),
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
// The catch-all is the only readable variant: any string, never `enum: []`.
#[case::other_only_unit(
    "other_only_unit",
    "enum Color { #[serde(skip_deserializing)] Red, #[serde(other)] Unknown }"
)]
#[case::other_only_external(
    "other_only_external",
    "enum Shape { #[serde(skip_deserializing)] Square(u8), #[serde(other)] Unknown }"
)]
#[case::other_only_internal(
    "other_only_internal",
    r#"#[serde(tag = "t")] enum Shape { #[serde(other)] Unknown }"#
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
// Round 9: an adjacently tagged `Option` newtype reads a missing content key
// as `None` (unless a variant `deserialize_with` reads it).
#[case::adjacent_option_content(
    "adjacent_option_content",
    r#"#[serde(tag = "t", content = "c")] enum E { A(Option<u8>), #[serde(deserialize_with = "d")] B(Option<u8>), C(u8) }"#
)]
// Round 9: an internally tagged `()` payload reads only the tag.
#[case::internal_unit_payload(
    "internal_unit_payload",
    r#"#[serde(tag = "type")] enum M { Unit(()) }"#
)]
// Round 9: an internally tagged `Option<Closed>` is `Closed` with the tag.
#[case::internal_option_closed(
    "internal_option_closed",
    r#"#[serde(tag = "type")] enum E { A(Option<Closed>), B(Option<Payload>) }"#
)]
// Round 9: positional fields keep their `#[schema]` / `#[garde]` rules and
// a tuple position its doc comment.
#[case::positional_field_rules(
    "positional_field_rules",
    r#"enum E { Score(#[schema(maximum = 10)] u8), Pair(#[garde(length(chars, min = 1))] String, #[doc = " Position."] #[deprecated] u8) }"#
)]
// Round 10: a tuple-variant position with `#[serde(default)]` may be
// omitted on input (trailing items are read from their defaults).
#[case::tuple_variant_defaults(
    "tuple_variant_defaults",
    "enum E { P(u8, #[serde(default)] u8), Q(#[serde(default)] u8, u8, #[serde(default)] u8) }"
)]
// Round 10: an adjacently tagged unit variant reads a present content key,
// accepting only `null` (open container too).
#[case::adjacent_unit_content(
    "adjacent_unit_content",
    r#"#[serde(tag = "t", content = "c")] enum E { U, S(u8) }"#
)]
// Round 10: an externally tagged payload of an internally tagged newtype
// sits next to the tag, so its branches lose `maxProperties: 1`.
#[case::internal_external_payload(
    "internal_external_payload",
    r#"#[serde(tag = "type")] enum E { A(Shape), B(Option<Shape>) }"#
)]
// Round 11: an internally tagged newtype over an externally tagged enum with
// unit variants: serde writes a unit variant as a key next to the tag
// (`{"type": "A", "Point": null}`), never as a string.
#[case::internal_external_unit_payload(
    "internal_external_unit_payload",
    r#"#[serde(tag = "type")] enum E { A(Kind), B(Mode) }"#
)]
// Round 11: a map payload's `length` bounds count the tag too.
#[case::internal_map_payload_bounds(
    "internal_map_payload_bounds",
    r#"#[serde(tag = "t")] enum E { M(#[garde(length(min = 1, max = 2))] std::collections::HashMap<String, u8>) }"#
)]
// Round 11: serde_json also reads a unit variant of a mixed externally
// tagged enum from its map form `{"Point": null}`.
#[case::external_unit_map_form(
    "external_unit_map_form",
    r#"enum Shape { #[serde(alias = "pt")] Point, Square(u8) }"#
)]
// Round 11: a tuple-variant position whose `skip_serializing_if` holds is
// left out of the written array.
#[case::tuple_variant_skip_serializing_if(
    "tuple_variant_skip_serializing_if",
    r#"enum E { V(u8, #[serde(skip_serializing_if = "Option::is_none")] Option<u8>) }"#
)]
// Round 15 L1: an externally tagged `#[serde(other)]` payload's map form
// lists the tag (also for an all-unit enum), and the payload map holds
// exactly one entry besides the tag.
#[case::internal_external_other_payload(
    "internal_external_other_payload",
    r#"#[serde(tag = "t")] enum E { V(Open), U(OpenUnit) }"#
)]
// Round 15 L2: serde checks a variant-level codec before the style, so a
// unit variant with one is a newtype variant on that side.
#[case::unit_variant_codec_external(
    "unit_variant_codec_external",
    r#"enum E { #[serde(serialize_with = "f")] Now, Other(u8) }"#
)]
#[case::unit_variant_codec_all_unit(
    "unit_variant_codec_all_unit",
    r#"enum E { #[serde(with = "m")] Now, Later }"#
)]
#[case::unit_variant_codec_internal(
    "unit_variant_codec_internal",
    r#"#[serde(tag = "t")] enum E { #[serde(deserialize_with = "f")] Now, Later }"#
)]
#[case::unit_variant_codec_adjacent(
    "unit_variant_codec_adjacent",
    r#"#[serde(tag = "t", content = "c")] enum E { #[serde(with = "m")] Now, Later }"#
)]
#[case::unit_variant_codec_untagged(
    "unit_variant_codec_untagged",
    r#"#[serde(untagged)] enum E { #[serde(with = "m")] Now, Later(u8) }"#
)]
fn enum_views(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(format!("enum_views_{name}"), both_views(src));
}
