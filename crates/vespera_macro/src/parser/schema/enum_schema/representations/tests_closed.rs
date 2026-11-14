//! Internally tagged newtype variants over closed (`deny_unknown_fields`)
//! payloads: serde accepts the tag next to the payload's fields, so the tag
//! must be part of the closed object — `allOf` with a closed member would
//! reject the tag itself.

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use crate::parser::schema::enum_schema::parse_enum_to_schema;

#[rstest]
#[case::closed_ref(
    "closed_ref",
    "#[serde(tag = \"type\")] enum Event { /// Created\n Created(Payload), Open(Other) }"
)]
#[case::closed_with_variant_rename(
    "closed_with_variant_rename",
    "#[serde(tag = \"kind\", rename_all = \"snake_case\")] enum Event { PayloadSent(Payload) }"
)]
#[case::unknown_ref("unknown_ref", "#[serde(tag = \"type\")] enum Event { Ext(External) }")]
#[case::borrowed_ref(
    "borrowed_ref",
    "#[serde(tag = \"type\")] enum Event { Borrowed(&'static Payload) }"
)]
#[case::closed_generic_inline(
    "closed_generic_inline",
    "#[serde(tag = \"type\")] enum Event { Wrapped(Closed<u8>), Map(std::collections::HashMap<String, u8>), Fieldless(Empty) }"
)]
// `value_type` documents (and classifies) the payload as the closed `Payload`.
#[case::value_type_closed(
    "value_type_closed",
    "#[serde(tag = \"type\")] enum Event { Created(#[schema(value_type = Payload)] Other) }"
)]
fn closed_newtype_payload_merges_the_tag(#[case] name: &str, #[case] src: &str) {
    let enum_item: syn::ItemEnum = syn::parse_str(src).unwrap();
    let known = HashSet::from([
        "Payload".to_string(),
        "Other".to_string(),
        "Closed".to_string(),
        "Empty".to_string(),
    ]);
    let definitions = HashMap::from([
        (
            "Payload".to_string(),
            "#[serde(deny_unknown_fields)] pub struct Payload { pub id: u32, pub note: Option<String> }"
                .to_string(),
        ),
        (
            "Other".to_string(),
            "pub struct Other { pub id: u32 }".to_string(),
        ),
        (
            "Closed".to_string(),
            "#[serde(deny_unknown_fields)] pub struct Closed<T> { pub v: T }".to_string(),
        ),
        (
            "Empty".to_string(),
            "#[serde(deny_unknown_fields)] pub struct Empty {}".to_string(),
        ),
    ]);
    let schema = parse_enum_to_schema(&enum_item, &known, &definitions);
    insta::assert_snapshot!(
        format!("internally_tagged_closed_{name}"),
        serde_json::to_string_pretty(&schema).unwrap()
    );
}

/// Enum-level `#[serde(deny_unknown_fields)]` closes every struct variant
/// (serde's struct-variant visitors read the enum's container attributes),
/// in every representation; adjacently tagged enums also reject keys other
/// than the tag and the content. Newtype payloads keep their own setting.
#[rstest]
#[case::external(
    "external",
    "#[serde(deny_unknown_fields)] enum E { S { a: u8 }, T(u8, u8), N(Open), U }"
)]
#[case::internal(
    "internal",
    "#[serde(tag = \"t\", deny_unknown_fields)] enum E { S { a: u8 }, N(Open), U }"
)]
#[case::adjacent(
    "adjacent",
    "#[serde(tag = \"t\", content = \"c\", deny_unknown_fields)] enum E { S { a: u8 }, U }"
)]
#[case::untagged(
    "untagged",
    "#[serde(untagged, deny_unknown_fields)] enum E { S { a: u8 }, N(Open) }"
)]
#[case::adjacent_open(
    "adjacent_open",
    "#[serde(tag = \"t\", content = \"c\")] enum E { S { a: u8 } }"
)]
fn enum_deny_unknown_fields_closes_struct_variants(#[case] name: &str, #[case] src: &str) {
    let enum_item: syn::ItemEnum = syn::parse_str(src).unwrap();
    let known = HashSet::from(["Open".to_string()]);
    let definitions = HashMap::from([(
        "Open".to_string(),
        "pub struct Open { pub id: u32 }".to_string(),
    )]);
    let schema = parse_enum_to_schema(&enum_item, &known, &definitions);
    insta::assert_snapshot!(
        format!("enum_deny_unknown_fields_{name}"),
        serde_json::to_string_pretty(&schema).unwrap()
    );
}
