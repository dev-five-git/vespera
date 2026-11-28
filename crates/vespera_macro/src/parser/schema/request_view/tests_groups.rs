//! Round-43 regression tests: a `$t:ty` fragment (`Type::Group`) and a
//! parenthesised `(T)` read as the type they wrap, in the request view's
//! `Option` / relation detection. A sidecar of `request_view`.

use std::collections::{BTreeSet, HashMap, HashSet};

use rstest::rstest;
use serde_json::json;

use super::super::struct_schema::parse_struct_to_schema;
use super::idents::referenced_idents;
use super::tests::kinds_by_ident;
use super::*;

/// `ty` wrapped the way `macro_rules!` wraps a `$t:ty` fragment.
fn invisible(ty: &str) -> syn::Type {
    syn::Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(syn::parse_str(ty).unwrap()),
    })
}

/// An `Option` written through a `$t:ty` fragment or `(..)` is the same
/// `Option`: its field differs per direction (serde accepts it absent on
/// input, and writes `null` on output).
#[rstest]
#[case::invisible_group(invisible("Option<u8>"))]
#[case::paren(syn::parse_str("(Option<u8>)").unwrap())]
fn option_through_groups_differs_per_direction(#[case] ty: syn::Type) {
    let item: syn::Item = syn::parse_quote!(struct A { y: #ty });
    let flat = FlatKinds::of(&[]);
    assert!(directly_differs(&item, &flat));
}

/// The request view of an `Option` through a `$t:ty` fragment accepts the key
/// absent, so it is not `required` (as for the bare `Option`).
#[rstest]
#[case::invisible_group(invisible("Option<u8>"))]
#[case::paren(syn::parse_str("(Option<u8>)").unwrap())]
fn option_through_groups_is_not_required_on_input(#[case] ty: syn::Type) {
    let item: syn::ItemStruct = syn::parse_quote!(struct S { y: #ty });
    let schema = with_request_view(|| {
        parse_struct_to_schema(
            &item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        )
    });
    let value = serde_json::to_value(schema).unwrap();
    let required = value["required"].as_array().cloned().unwrap_or_default();
    assert!(!required.contains(&json!("y")), "{value}");
}

/// A `BelongsTo<user::Entity>` field through a `$t:ty` fragment references its
/// target component, as the bare relation does.
#[test]
fn relation_through_group_references_its_target() {
    let relation = invisible("BelongsTo<user::Entity>");
    let item: syn::Item = syn::parse_quote!(struct Post { author: #relation });
    let known = BTreeSet::from(["User".to_string()]);
    assert!(referenced_idents(&item, &known).contains("User"));
}

/// `struct W<T>(Option<T>)` reads like its argument, as a flattened `Option<T>`
/// does: `W<HashMap<..>>` is a map (every key), `W<Page>` leaves only its own.
#[test]
fn generic_option_newtype_reads_like_its_argument() {
    let items: Vec<syn::Item> = [
        "struct Wrap<T>(Option<T>);",
        "struct Holds { #[serde(flatten)] w: Wrap<HashMap<String, u8>> }",
        "struct Page { page: u32 }",
        "struct Keeps { #[serde(flatten)] w: Wrap<Page> }",
    ]
    .iter()
    .map(|src| syn::parse_str(src).unwrap())
    .collect();
    let kinds = kinds_by_ident(&items);
    assert_eq!(kinds.get("Holds"), Some(&FlatKind::Map));
    assert_eq!(
        kinds.get("Keeps"),
        Some(&FlatKind::Leaves { known_keys: true })
    );
}
