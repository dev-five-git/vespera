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
// L3 / L5: sets only promise unique items on output; std `Duration` is also
// read from `[secs, nanos]`.
#[case::sets("sets", "struct S { tags: HashSet<String>, ids: BTreeSet<u8> }")]
#[case::std_duration("std_duration", "struct S { ttl: std::time::Duration }")]
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
    install(Arc::new(BTreeMap::from([
        ("Page".to_string(), "PageInput".to_string()),
        ("Holder".to_string(), "HolderInput".to_string()),
    ])));
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
