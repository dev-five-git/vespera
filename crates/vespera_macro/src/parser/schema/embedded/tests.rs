//! Embedded schemas never reject the keys serde keeps from them: a flattened
//! member's outer keys, an internally tagged payload's tag.

use std::collections::{HashMap, HashSet};

use rstest::rstest;
use vespera_core::schema::{Schema, SchemaRef};

use super::*;
use crate::parser::schema::{
    generic_instances, parse_enum_to_schema, parse_struct_to_schema,
    request_view::with_request_view,
};

const DEFINITIONS: &[(&str, &str)] = &[
    (
        "Strict",
        "#[serde(deny_unknown_fields)] struct Strict { token: String }",
    ),
    ("U", "#[serde(untagged)] enum U { S(Strict), B { b: u8 } }"),
    (
        "M",
        "#[serde(untagged)] enum M { M(HashMap<String, u8>), B { b: u8 } }",
    ),
    ("Shape", "enum Shape { Square(u8), Point }"),
    ("W", "struct W(Shape);"),
    ("DescribedW", "/// Shape wrapper\nstruct DescribedW(W);"),
    ("UE", "#[serde(untagged)] enum UE { E(Shape) }"),
    (
        "Rec",
        "#[serde(untagged)] enum Rec { S(Strict), N(Box<Rec>) }",
    ),
    (
        "Adj",
        r#"#[serde(tag = "t", content = "c", deny_unknown_fields)] enum Adj { A(u8) }"#,
    ),
    (
        "Int",
        r#"#[serde(tag = "kind", deny_unknown_fields)] enum Int { A { x: u8 } }"#,
    ),
    (
        "Unt",
        "#[serde(untagged, deny_unknown_fields)] enum Unt { A { x: u8 } }",
    ),
    ("StrictW", "struct StrictW(Strict);"),
    (
        "Tree",
        "#[serde(deny_unknown_fields)] struct Tree<T> { v: T, kids: Vec<Tree<T>> }",
    ),
    ("Loop", "struct Loop(Box<Loop>);"),
];

fn lookups() -> (HashSet<String>, HashMap<String, String>) {
    let definitions: HashMap<String, String> = DEFINITIONS
        .iter()
        .map(|(name, def)| ((*name).to_string(), (*def).to_string()))
        .collect();
    let mut known: HashSet<String> = definitions.keys().cloned().collect();
    known.insert("Ghost".to_string());
    (known, definitions)
}

/// `response:` / `request:` JSON of the struct or enum `src`.
fn both_views(src: &str) -> String {
    let (known, definitions) = lookups();
    let render = || {
        syn::parse_str::<syn::ItemStruct>(src).map_or_else(
            |_| parse_enum_to_schema(&syn::parse_str(src).unwrap(), &known, &definitions),
            |item| parse_struct_to_schema(&item, &known, &definitions),
        )
    };
    let response = render();
    let request = with_request_view(render);
    generic_instances::take_materialized();
    format!(
        "{src}\nresponse: {}\nrequest: {}",
        serde_json::to_string_pretty(&response).unwrap(),
        serde_json::to_string_pretty(&request).unwrap()
    )
}

/// What serde itself reads and writes in each of the snapshotted shapes:
/// the embedded value never sees the outer keys / the tag.
#[test]
#[allow(
    dead_code,
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::use_self
)]
fn serde_hides_the_other_keys_from_embedded_values() {
    use serde::{Deserialize, Serialize};
    use serde_json::{from_str, json, to_value};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        token: String,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U {
        S(Strict),
        B { b: u8 },
    }
    #[derive(Deserialize)]
    struct FlatU {
        id: i32,
        #[serde(flatten)]
        u: U,
    }
    assert!(from_str::<FlatU>(r#"{"id":1,"token":"x"}"#).is_ok());

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum M {
        M(HashMap<String, u8>),
        B { b: u8 },
    }
    #[derive(Deserialize)]
    struct FlatM {
        name: String,
        #[serde(flatten)]
        m: M,
    }
    assert!(from_str::<FlatM>(r#"{"name":"x","k":1}"#).is_ok());

    #[derive(Serialize, Deserialize)]
    enum Shape {
        Square(u8),
        Point,
    }
    #[derive(Serialize, Deserialize)]
    struct W(Shape);
    #[derive(Serialize, Deserialize)]
    struct FlatW {
        id: i32,
        #[serde(flatten)]
        w: W,
    }
    assert!(from_str::<FlatW>(r#"{"id":1,"Square":3}"#).is_ok());
    assert!(from_str::<FlatW>(r#"{"id":1,"Point":null}"#).is_ok());
    assert!(from_str::<FlatW>(r#"{"id":1,"Point":null,"Square":3}"#).is_ok());
    let point = FlatW {
        id: 1,
        w: W(Shape::Point),
    };
    assert_eq!(to_value(&point).unwrap(), json!({"id": 1, "Point": null}));

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum UE {
        E(Shape),
    }
    #[derive(Deserialize)]
    struct FlatUE {
        id: i32,
        #[serde(flatten)]
        u: UE,
    }
    assert!(from_str::<FlatUE>(r#"{"id":1,"Square":3}"#).is_ok());
    assert!(from_str::<FlatUE>(r#"{"id":1,"Point":null}"#).is_ok());

    #[derive(Deserialize)]
    #[serde(tag = "t", content = "c", deny_unknown_fields)]
    enum Adj {
        A(u8),
    }
    #[derive(Deserialize)]
    #[serde(tag = "kind", deny_unknown_fields)]
    enum Int {
        A { x: u8 },
    }
    #[derive(Deserialize)]
    #[serde(untagged, deny_unknown_fields)]
    enum Unt {
        A { x: u8 },
    }
    #[derive(Deserialize)]
    struct StrictW(Strict);
    #[derive(Deserialize)]
    #[serde(tag = "type")]
    enum Outer {
        X(Adj),
        Y(Int),
        Z(Unt),
        W(W),
        S(StrictW),
    }
    for value in [
        r#"{"type":"X","t":"A","c":1}"#,
        r#"{"type":"Y","kind":"A","x":1}"#,
        r#"{"type":"Z","x":1}"#,
        r#"{"type":"W","Square":3}"#,
        r#"{"type":"W","Point":null}"#,
        r#"{"type":"S","token":"x"}"#,
    ] {
        assert!(from_str::<Outer>(value).is_ok(), "{value}");
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Tree<T> {
        v: T,
        kids: Vec<Tree<T>>,
    }
    #[derive(Deserialize)]
    #[serde(tag = "t")]
    enum E {
        A(Tree<u8>),
    }
    assert!(from_str::<E>(r#"{"t":"A","v":1,"kids":[]}"#).is_ok());
}

#[rstest]
// Round 16 S-M1: a closed `$ref` variant and a map variant of a flattened
// untagged enum.
#[case::flatten_untagged_closed_ref(
    "flatten_untagged_closed_ref",
    "struct Req { id: i32, #[serde(flatten)] u: U }"
)]
#[case::flatten_untagged_map(
    "flatten_untagged_map",
    "struct Req { name: String, #[serde(flatten)] m: M }"
)]
// Round 16 S-M2: a newtype wrapper (described or not) around an externally
// tagged enum is flattened like the enum.
#[case::flatten_newtype_enum(
    "flatten_newtype_enum",
    "struct Req { id: i32, #[serde(flatten)] w: W }"
)]
#[case::flatten_described_wrappers(
    "flatten_described_wrappers",
    "struct Req { id: i32, #[serde(flatten)] w: DescribedW }"
)]
// A nested externally tagged enum (an untagged variant) loses its single-key
// bound, and its unit variant is its key.
#[case::flatten_untagged_nested_external(
    "flatten_untagged_nested_external",
    "struct Req { id: i32, #[serde(flatten)] u: UE }"
)]
// A recursive variant keeps its `$ref` the second time.
#[case::flatten_untagged_recursive(
    "flatten_untagged_recursive",
    "struct Req { id: i32, #[serde(flatten)] r: Rec }"
)]
// Round 16 E-M1: an enum payload's closed branches list the tag; a wrapper
// payload is read like the value it wraps.
#[case::internally_tagged_enum_payloads(
    "internally_tagged_enum_payloads",
    r#"#[serde(tag = "type")] enum Outer { X(Adj), Y(Int), Z(Unt), W(W), S(StrictW), G(Ghost) }"#
)]
// Round 16 E-L1: a closed recursive generic payload.
#[case::internally_tagged_recursive_generic(
    "internally_tagged_recursive_generic",
    r#"#[serde(tag = "t")] enum E { A(Tree<u8>) }"#
)]
fn embedded_schemas_accept_the_other_keys(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(format!("embedded_{name}"), both_views(src));
}

/// A wrapper is only `allOf: [$ref]` plus a description; a wrapper of
/// itself, or of an unknown component, stays as it is.
#[test]
fn unwrap_wrappers_stops_at_non_wrappers() {
    let (known, definitions) = lookups();
    let wrapper = |name: &str| Schema {
        all_of: Some(vec![SchemaRef::Ref(Reference::schema(name))]),
        ..Schema::default()
    };
    let unwrapped = |schema: Schema| {
        serde_json::to_value(unwrap_wrappers(schema, &known, &definitions)).unwrap()
    };
    let nullable = Schema {
        nullable: Some(true),
        ..wrapper("Shape")
    };
    let two_members = Schema {
        all_of: Some(vec![
            SchemaRef::Ref(Reference::schema("Shape")),
            SchemaRef::Inline(Box::default()),
        ]),
        ..Schema::default()
    };
    for kept in [wrapper("Ghost"), nullable, two_members] {
        let before = serde_json::to_value(&kept).unwrap();
        assert_eq!(unwrapped(kept), before);
    }
    assert_eq!(
        unwrapped(wrapper("Loop")),
        serde_json::to_value(wrapper("Loop")).unwrap()
    );
    assert!(unwrapped(wrapper("W"))["oneOf"].is_array());
}

/// An unknown `$ref` branch is kept, and nothing changes.
#[test]
fn open_embedded_keeps_unknown_refs() {
    let (known, definitions) = lookups();
    let schema = Schema {
        any_of: Some(vec![SchemaRef::Ref(Reference::schema("Ghost"))]),
        ..Schema::default()
    };
    assert!(open_embedded(&schema, &without_unlisted_bounds, &known, &definitions).is_none());
}
