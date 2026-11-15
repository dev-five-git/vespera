//! `#[serde(transparent)]` picks its field per side exactly like
//! serde_derive's `check_transparent`: `PhantomData` never counts, a
//! `skip_serializing` field is not written, a `skip_deserializing` /
//! `default` field is not read.

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use super::*;
use crate::parser::schema::request_view::with_request_view;

/// `response:` / `request:` JSON of `src`.
fn both_views(src: &str) -> String {
    let item: syn::ItemStruct = syn::parse_str(src).unwrap();
    let (known, definitions) = (HashSet::<String>::new(), HashMap::<String, String>::new());
    let response = parse_struct_to_schema(&item, &known, &definitions);
    let request = with_request_view(|| parse_struct_to_schema(&item, &known, &definitions));
    format!(
        "{src}\nresponse: {}\nrequest: {}",
        serde_json::to_string_pretty(&response).unwrap(),
        serde_json::to_string_pretty(&request).unwrap()
    )
}

#[rstest]
#[case::phantom_not_skipped(
    "phantom_not_skipped",
    "#[serde(transparent)] struct W { v: u32, marker: PhantomData<u8> }"
)]
#[case::per_side(
    "per_side",
    "#[serde(transparent)] struct W { #[serde(skip_deserializing)] out: u32, #[serde(skip_serializing)] inp: String }"
)]
#[case::default_not_read(
    "default_not_read",
    "#[serde(transparent)] struct W { v: u32, #[serde(default, skip_serializing)] extra: String }"
)]
// Positional / single-field payloads keep their `#[schema]` and `#[garde]`
// rules (the struct doc comment still describes the whole value).
#[case::newtype_constraints(
    "newtype_constraints",
    "/// An age.\nstruct Age(#[schema(minimum = 0, maximum = 150)] u8);"
)]
#[case::transparent_rules(
    "transparent_rules",
    "#[serde(transparent)] struct Email { #[garde(email)] #[schema(max_length = 64)] v: String }"
)]
#[case::tuple_positions(
    "tuple_positions",
    r#"struct Pair(#[schema(min_length = 1)] String, #[doc = " The count."] #[garde(range(max = 9))] u8);"#
)]
fn transparent_views(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(format!("transparent_views_{name}"), both_views(src));
}

/// serde ungroups the type before its `PhantomData` test (a `Type::Group`
/// comes from a `macro_rules!` expansion).
#[test]
fn grouped_phantom_data_is_recognised() {
    let grouped = syn::Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(syn::parse_quote!(std::marker::PhantomData<u8>)),
    });
    assert!(is_phantom_data(&grouped));
    assert!(!is_phantom_data(&syn::parse_quote!(&PhantomData<u8>)));
    assert!(!is_phantom_data(&syn::parse_quote!(u8)));
}
