//! Round-37 regression tests: external-crate paths never resolve to a local
//! item by its bare name. A sidecar of `request_view` (helpers from `tests`).

use std::collections::BTreeMap;
use std::sync::Arc;

use rstest::rstest;

use super::tests::{kinds_by_ident, scoped_names};
use super::*;

/// Round 37: an external-crate path (`serde_json::Value`) names no crate-local
/// `Value`, so neither the owner lookup nor `mentions_input` falls back to the
/// bare ident for it.
#[test]
fn external_crate_paths_never_fall_back_to_a_local_bare_name() {
    let result = scoped_names(
        &[
            (
                "Value",
                Some("Value"),
                None,
                "struct Value { #[serde(default)] x: u8 }",
            ),
            ("Req", None, None, "struct Req { v: serde_json::Value }"),
        ],
        &[],
    );
    assert!(
        result.iter().any(|(component, _)| component == "Value"),
        "{result:?}"
    );
    assert!(
        !result.iter().any(|(component, _)| component == "Req"),
        "{result:?}"
    );

    install(Arc::new(
        BTreeMap::from([("Value".to_string(), "ValueInput".to_string())]).into(),
    ));
    let external: [syn::Type; 1] = [syn::parse_quote!(serde_json::Value)];
    let local: [syn::Type; 1] = [syn::parse_quote!(Value)];
    assert!(!mentions_input(&external));
    assert!(mentions_input(&local));
    install(Arc::default());
}

/// Round 37: a flattened `serde_json::Value` is not the local `Value` that
/// reads every key, so `Outer` does not become a map-reading type through it.
#[test]
fn external_flattened_type_is_not_a_local_item() {
    let items: Vec<syn::Item> = vec![
        syn::parse_quote!(
            struct Value {
                #[serde(flatten)]
                rest: std::collections::HashMap<String, u8>,
            }
        ),
        syn::parse_quote!(
            struct Outer {
                #[serde(flatten)]
                v: serde_json::Value,
            }
        ),
    ];
    let kinds = kinds_by_ident(&items);
    assert!(
        matches!(kinds.get("Value"), Some(FlatKind::Map)),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.get("Outer"),
        Some(&FlatKind::Leaves { known_keys: true }),
        "{kinds:?}"
    );
}

/// A module-qualified type names its registry key only: its module segments
/// (`other`, `user`, `crate`) name no type, so a component called `user` is
/// no dependency of a field typed `other::user::Thing`.
#[rstest]
#[case::module_path("struct A { x: other::user::Thing }", &["other::user::Thing"])]
#[case::anchored_path("struct A { x: crate::models::User }", &["crate::models::User"])]
fn module_segments_are_not_referenced_idents(#[case] src: &str, #[case] expected: &[&str]) {
    let item: syn::Item = syn::parse_str(src).unwrap();
    let found = referenced_idents(&item, &BTreeSet::new());
    assert_eq!(
        found.iter().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
}

/// Round 40 (P2-L1): a type named by a module-scoped key that is registered
/// decides by that key alone. `b::Foo` (no input view) names no input even
/// though the bare `Foo` is `a::Foo`'s renamed component with one, so the
/// bare-ident fallback must not reach it. An unregistered scope still falls
/// back to the bare ident.
#[test]
fn registered_scoped_key_decides_before_the_bare_ident() {
    let items: [syn::Item; 2] = [
        syn::parse_quote!(
            struct Foo {
                #[serde(default)]
                a: u8,
            }
        ),
        syn::parse_quote!(
            struct Foo {
                b: u8,
            }
        ),
    ];
    let sources = [
        ViewSource {
            name: "A",
            rust_ident: Some("Foo"),
            scoped_keys: vec!["a::Foo"],
            item: &items[0],
        },
        ViewSource {
            name: "B",
            rust_ident: Some("Foo"),
            scoped_keys: vec!["b::Foo"],
            item: &items[1],
        },
    ];
    install(compute_input_names(&sources, &BTreeSet::new()));

    let registered_without_input: [syn::Type; 1] = [syn::parse_quote!(b::Foo)];
    let registered_with_input: [syn::Type; 1] = [syn::parse_quote!(a::Foo)];
    let unregistered_scope: [syn::Type; 1] = [syn::parse_quote!(c::Foo)];
    assert!(!mentions_input(&registered_without_input));
    assert!(mentions_input(&registered_with_input));
    assert!(mentions_input(&unregistered_scope));
    install(Arc::default());
}
