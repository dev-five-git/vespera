//! Round-37 regression tests: external-crate paths never resolve to a local
//! item by its bare name. A sidecar of `request_view` (helpers from `tests`).

use std::collections::BTreeMap;
use std::sync::Arc;

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
