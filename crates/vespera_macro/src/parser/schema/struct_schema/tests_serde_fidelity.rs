//! The schema of a struct accepts exactly what serde writes (response view)
//! and reads (request view): `flatten` merging, `alias`, tuple codecs and
//! one-sided skips, sets, `Duration`. Each snapshot holds both views.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use rstest::rstest;

use super::*;
use crate::parser::schema::request_view::{install, with_request_view};

const DEFINITIONS: &[(&str, &str)] = &[
    (
        "Page",
        "struct Page { page: u32, #[serde(default)] size: u32 }",
    ),
    (
        "Strict",
        "#[serde(deny_unknown_fields)] struct Strict { token: String }",
    ),
    (
        "Mid",
        "struct Mid { mid: u8, #[serde(flatten)] strict: Strict }",
    ),
    (
        "Kind",
        r#"#[serde(tag = "kind")] enum Kind { A { x: u8 }, B }"#,
    ),
    ("Marker", "struct Marker;"),
    (
        "Named",
        r#"#[schema(name = "NamedDto")] struct Named { n: u8 }"#,
    ),
    (
        "Aliased",
        r#"struct Aliased { #[serde(alias = "nm")] name: String }"#,
    ),
    (
        "Node",
        "struct Node { id: i32, #[serde(flatten)] next: Option<Box<Node>> }",
    ),
    ("Audit", "struct Audit { by: String }"),
    (
        "Meta",
        "struct Meta { tag: String, #[serde(flatten)] audit: Audit }",
    ),
    (
        "Tagged",
        "struct Tagged { t: u8, #[serde(flatten)] kind: Kind }",
    ),
    ("Tree", "struct Tree<T> { v: T, kids: Vec<Tree<T>> }"),
    ("Shape", "enum Shape { Square(u8), Point }"),
    ("Wrapper", "struct Wrapper(Strict);"),
    ("Attrs", "struct Attrs(HashMap<String, u8>);"),
    ("Open", "enum Open { Square(u8), #[serde(other)] Unknown }"),
    (
        "Transparent",
        "#[serde(transparent)] struct Transparent { inner: Strict }",
    ),
    (
        "StrictKind",
        r#"#[serde(tag = "kind", deny_unknown_fields)] enum StrictKind { A { x: u8 }, B { y: u8 } }"#,
    ),
    (
        "StrictUntagged",
        "#[serde(untagged, deny_unknown_fields)] enum StrictUntagged { A { x: u8 }, B { y: String } }",
    ),
    (
        "StrictTree",
        "#[serde(deny_unknown_fields)] struct StrictTree<T> { v: T, kids: Vec<StrictTree<T>> }",
    ),
];

fn known() -> HashSet<String> {
    let mut known: HashSet<String> = DEFINITIONS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    known.insert("Ghost".to_string());
    known
}

fn definitions() -> HashMap<String, String> {
    DEFINITIONS
        .iter()
        .map(|(name, def)| ((*name).to_string(), (*def).to_string()))
        .collect()
}

/// `response:` / `request:` JSON of `src`.
fn both_views(src: &str) -> String {
    let item: syn::ItemStruct = syn::parse_str(src).unwrap();
    let (known, definitions) = (known(), definitions());
    let response = parse_struct_to_schema(&item, &known, &definitions);
    let request = with_request_view(|| parse_struct_to_schema(&item, &known, &definitions));
    super::super::generic_instances::take_materialized();
    format!(
        "{src}\nresponse: {}\nrequest: {}",
        serde_json::to_string_pretty(&response).unwrap(),
        serde_json::to_string_pretty(&request).unwrap()
    )
}

#[rstest]
// H1: a flattened map types the keys the struct does not list itself.
#[case::flatten_map(
    "flatten_map",
    "struct Req { id: i32, #[serde(flatten)] extra: HashMap<String, String> }"
)]
#[case::flatten_map_and_struct(
    "flatten_map_and_struct",
    "struct Req { id: i32, #[serde(flatten)] page: Page, #[serde(flatten)] extra: BTreeMap<String, i64> }"
)]
// A map declared BEFORE a struct also reads the struct's keys (request view).
#[case::flatten_map_before_struct(
    "flatten_map_before_struct",
    "struct Req { id: i32, #[serde(flatten)] extra: BTreeMap<String, i64>, #[serde(flatten)] page: Page }"
)]
#[case::flatten_two_maps_before_optional_struct(
    "flatten_two_maps_before_optional_struct",
    "struct Req { #[serde(flatten)] a: HashMap<String, u8>, #[serde(flatten)] b: HashMap<String, u16>, #[serde(flatten)] page: Option<Page> }"
)]
#[case::flatten_optional_map_before_struct(
    "flatten_optional_map_before_struct",
    "struct Req { #[serde(flatten)] extra: Option<HashMap<String, String>>, #[serde(flatten)] page: Page }"
)]
#[case::flatten_map_deny(
    "flatten_map_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] extra: HashMap<String, String> }"
)]
#[case::flatten_two_maps(
    "flatten_two_maps",
    "struct Req { #[serde(flatten)] a: HashMap<String, u8>, #[serde(flatten)] b: HashMap<String, u16> }"
)]
#[case::flatten_map_and_enum(
    "flatten_map_and_enum",
    "struct Req { id: i32, #[serde(flatten)] kind: Kind, #[serde(flatten)] extra: HashMap<String, String> }"
)]
#[case::flatten_value_type_map(
    "flatten_value_type_map",
    "struct Req { id: i32, #[serde(flatten)] #[schema(value_type = HashMap<String, u8>)] extra: Custom }"
)]
// Round 14: `value_type` wins over a custom codec on a flattened field too.
#[case::flatten_value_type_over_codec(
    "flatten_value_type_over_codec",
    "struct Req { id: i32, #[serde(flatten, with = \"m\")] #[schema(value_type = HashMap<String, u8>)] extra: Custom }"
)]
// H2: a flattened `deny_unknown_fields` struct never rejects the outer keys.
#[case::flatten_closed_struct(
    "flatten_closed_struct",
    "struct Req { id: i32, #[serde(flatten)] strict: Strict }"
)]
#[case::flatten_closed_struct_outer_deny(
    "flatten_closed_struct_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] strict: Strict }"
)]
#[case::flatten_nested_closed_outer_deny(
    "flatten_nested_closed_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] mid: Mid }"
)]
#[case::flatten_plain_struct_keeps_ref(
    "flatten_plain_struct_keeps_ref",
    "struct Req { id: i32, #[serde(flatten)] page: Page }"
)]
#[case::flatten_plain_struct_outer_deny(
    "flatten_plain_struct_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] meta: Meta }"
)]
// Round 18 M-A: `Meta` (it flattens `Audit`) and the internally tagged
// `Kind` leave their keys in serde's buffer: the outer `deny_unknown_fields`
// rejects them, and a map declared after them reads them.
#[case::flatten_leaving_struct_then_map(
    "flatten_leaving_struct_then_map",
    "struct Req { #[serde(flatten)] meta: Meta, #[serde(flatten)] extra: HashMap<String, u8> }"
)]
#[case::flatten_leaving_optional_outer_deny(
    "flatten_leaving_optional_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] meta: Option<Meta> }"
)]
#[case::flatten_leaving_enum_outer_deny(
    "flatten_leaving_enum_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] kind: Kind }"
)]
// Round 18 M-B: a flattened newtype around a map reads every buffered key.
#[case::flatten_newtype_map_outer_deny(
    "flatten_newtype_map_outer_deny",
    "#[serde(deny_unknown_fields)] struct Req { id: u8, #[serde(flatten)] attrs: Attrs }"
)]
// M5: a flattened unit struct contributes nothing.
#[case::flatten_unit_struct(
    "flatten_unit_struct",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] marker: Marker }"
)]
// M8: a flattened `Option` never requires a key, resolved by Rust type.
#[case::flatten_option_renamed_component(
    "flatten_option_renamed_component",
    "struct Req { id: i32, #[serde(flatten)] named: Option<Named> }"
)]
#[case::flatten_option_enum(
    "flatten_option_enum",
    "struct Req { id: i32, #[serde(flatten)] kind: Option<Kind> }"
)]
#[case::flatten_option_unresolvable(
    "flatten_option_unresolvable",
    "struct Req { id: i32, #[serde(flatten)] ghost: Option<Ghost> }"
)]
#[case::flatten_option_nested_ref(
    "flatten_option_nested_ref",
    "struct Req { #[serde(flatten)] meta: Option<Meta> }"
)]
#[case::flatten_option_with_unknown_member(
    "flatten_option_with_unknown_member",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] tagged: Option<Tagged> }"
)]
#[case::flatten_option_closed(
    "flatten_option_closed",
    "struct Req { id: i32, #[serde(flatten)] strict: Option<Strict> }"
)]
// L6: flattening itself through `Option<Box<Self>>` terminates.
#[case::flatten_self_option(
    "flatten_self_option",
    "struct Node { id: i32, #[serde(flatten)] next: Option<Box<Node>> }"
)]
#[case::flatten_recursive_generic(
    "flatten_recursive_generic",
    "struct Req { id: i32, #[serde(flatten)] tree: Tree<u8> }"
)]
// Round 15 M1: a closed member that is not a plain struct never rejects the
// outer keys either (serde never shows them to it).
#[case::flatten_newtype_closed(
    "flatten_newtype_closed",
    "struct Req { id: i32, #[serde(flatten)] w: Wrapper }"
)]
#[case::flatten_transparent_closed(
    "flatten_transparent_closed",
    "struct Req { id: i32, #[serde(flatten)] w: Transparent }"
)]
#[case::flatten_internal_enum_closed(
    "flatten_internal_enum_closed",
    "struct Req { id: i32, #[serde(flatten)] kind: StrictKind }"
)]
#[case::flatten_untagged_enum_closed(
    "flatten_untagged_enum_closed",
    "struct Req { id: i32, #[serde(flatten)] u: StrictUntagged }"
)]
#[case::flatten_recursive_generic_closed(
    "flatten_recursive_generic_closed",
    "struct Req { id: i32, #[serde(flatten)] tree: StrictTree<u8> }"
)]
// Round 15 L1: a flattened enum never reads an unknown key as its
// `#[serde(other)]` variant.
#[case::flatten_other_enum(
    "flatten_other_enum",
    "struct Req { a: Option<String>, #[serde(flatten)] o: Open }"
)]
// Round 15 L3: a boxed flattened `Option` is still optional.
#[case::flatten_boxed_option(
    "flatten_boxed_option",
    "struct Req { id: i32, #[serde(flatten)] page: Box<Option<Page>> }"
)]
#[case::flatten_custom_codec(
    "flatten_custom_codec",
    r#"struct Req { id: i32, #[serde(flatten, with = "m")] extra: Extra }"#
)]
#[case::flatten_scalar(
    "flatten_scalar",
    "struct Req { id: i32, #[serde(flatten)] #[schema(value_type = String)] raw: Custom }"
)]
// M1: aliases are accepted keys of the request view.
#[case::alias_fields(
    "alias_fields",
    r#"#[serde(deny_unknown_fields)] struct A { #[serde(alias = "nm", alias = "n")] name: String, #[serde(alias = "yrs", default)] age: u8, #[serde(alias = "ident")] id: u64, note: Option<String> }"#
)]
#[case::alias_single_group(
    "alias_single_group",
    r#"struct A { #[serde(alias = "nm")] name: String, id: u8 }"#
)]
// Round 15: an optional aliased field still accepts only one of its names.
#[case::alias_optional(
    "alias_optional",
    r#"struct A { #[serde(alias = "nm")] name: Option<String>, #[serde(alias = "a", alias = "b", default)] x: u8 }"#
)]
// Round 16 S-L1: an alias equal to the wire name, or repeated, is that one
// name (see `serde_accepts_duplicate_aliases`).
#[case::alias_duplicates(
    "alias_duplicates",
    r#"#[serde(rename_all = "camelCase")] struct A { #[serde(alias = "userId")] user_id: u64, #[serde(alias = "a", alias = "a")] b: Option<u8>, #[serde(alias = "c", alias = "c")] d: u8 }"#
)]
#[case::flatten_aliased(
    "flatten_aliased",
    "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] a: Aliased }"
)]
#[case::flatten_option_aliased(
    "flatten_option_aliased",
    "struct Req { id: i32, #[serde(flatten)] a: Option<Aliased> }"
)]
// M9: a `skip_serializing_if` field is optional on output only.
#[case::skip_serializing_if(
    "skip_serializing_if",
    r#"struct S { #[serde(skip_serializing_if = "Vec::is_empty")] v: Vec<u8> }"#
)]
// M6 / L1: tuple positions honour codecs, `value_type` and one-sided skips.
#[case::tuple_codecs(
    "tuple_codecs",
    r#"struct P(#[serde(with = "ts")] DateTime<Utc>, #[schema(value_type = String)] Id, #[serde(serialize_with = "f")] u8);"#
)]
#[case::tuple_one_sided_skips(
    "tuple_one_sided_skips",
    "struct P(u8, #[serde(skip_serializing)] u16, #[serde(skip_deserializing)] u32);"
)]
// Round 11: serde leaves out a tuple position whose `skip_serializing_if`
// holds, shifting later elements left (`P(1, None, 2)` → `[1, 2]`).
#[case::tuple_skip_serializing_if(
    "tuple_skip_serializing_if",
    r#"struct P(u8, #[serde(skip_serializing_if = "Option::is_none")] Option<u16>, u32);"#
)]
#[case::tuple_trailing_skip_serializing_if(
    "tuple_trailing_skip_serializing_if",
    r#"struct P(u8, #[serde(skip_serializing_if = "Option::is_none")] Option<u16>);"#
)]
// L3 / L5: sets only promise unique items on output; std `Duration` is also
// read from `[secs, nanos]`.
#[case::sets("sets", "struct S { tags: HashSet<String>, ids: BTreeSet<u8> }")]
#[case::std_duration("std_duration", "struct S { ttl: std::time::Duration }")]
// Round 10: defaulted tuple positions (or a container `default`) may be
// omitted on input.
#[case::tuple_position_defaults(
    "tuple_position_defaults",
    "struct Range(u32, #[serde(default)] u32, #[serde(skip)] u8);"
)]
#[case::tuple_container_default(
    "tuple_container_default",
    "#[serde(default)] struct Range(u32, u32);"
)]
// Round 10: a flattened externally tagged enum's variant key sits among the
// struct's keys (no `maxProperties: 1`).
#[case::flatten_external_enum(
    "flatten_external_enum",
    "struct Req { id: i32, #[serde(flatten)] shape: Shape }"
)]
// M4: `transparent` after another key.
#[case::transparent_after_crate(
    "transparent_after_crate",
    r#"#[serde(crate = "my_serde", transparent)] struct W { v: u32 }"#
)]
fn struct_views(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(format!("struct_views_{name}"), both_views(src));
}

/// A flattened `Option` of a type whose request view is an `Input`
/// component is merged from its Rust type (the `$ref` names `PageInput`),
/// and a kept `$ref` member of a flattened struct maps back from its
/// `Input` name.
#[test]
fn flattened_input_components_are_resolved() {
    install(Arc::new(
        BTreeMap::from([
            ("Page".to_string(), "PageInput".to_string()),
            ("Holder".to_string(), "HolderInput".to_string()),
        ])
        .into(),
    ));
    let mut definitions = definitions();
    definitions.insert(
        "Holder".to_string(),
        "struct Holder { h: u8, #[serde(flatten)] page: Page }".to_string(),
    );
    let mut known = known();
    known.insert("Holder".to_string());
    let render = |src: &str| {
        let item: syn::ItemStruct = syn::parse_str(src).unwrap();
        serde_json::to_string_pretty(&with_request_view(|| {
            parse_struct_to_schema(&item, &known, &definitions)
        }))
        .unwrap()
    };
    let optional = render("struct Req { id: i32, #[serde(flatten)] page: Option<Page> }");
    let nested = render(
        "#[serde(deny_unknown_fields)] struct Req { id: i32, #[serde(flatten)] holder: Holder }",
    );
    install(Arc::default());
    insta::assert_snapshot!(
        "struct_views_flattened_input_components",
        format!("{optional}\n{nested}")
    );
}

/// serde_derive keeps a field's accepted names in a set: an alias equal to
/// the wire name, or given twice, compiles and is just that name.
#[test]
#[allow(dead_code, clippy::duplicated_attributes)]
fn serde_accepts_duplicate_aliases() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        #[serde(alias = "userId")]
        user_id: u64,
        #[serde(alias = "a", alias = "a")]
        b: Option<u8>,
    }
    assert!(serde_json::from_str::<A>(r#"{"userId":1}"#).is_ok());
    assert!(serde_json::from_str::<A>(r#"{"userId":1,"a":2}"#).is_ok());
    assert!(serde_json::from_str::<A>(r#"{"userId":1,"a":2,"b":3}"#).is_err());
}
