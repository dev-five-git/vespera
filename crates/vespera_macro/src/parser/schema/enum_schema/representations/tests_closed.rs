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
#[case::closed_generic_inline(
    "closed_generic_inline",
    "#[serde(tag = \"type\")] enum Event { Wrapped(Closed<u8>), Map(std::collections::HashMap<String, u8>), Fieldless(Empty) }"
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
