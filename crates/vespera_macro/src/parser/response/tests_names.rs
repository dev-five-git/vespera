//! Round-40 P1-L1: a crate type reached through a module that only re-exports
//! it is this crate's type (sidecar of `response`; the wrapper name is the
//! external one's).

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use super::*;

fn return_type(text: &str) -> syn::ReturnType {
    syn::parse_str::<syn::Signature>(&format!("fn test() {text}"))
        .expect("signature")
        .output
}

/// `mod api { pub use crate::dto::Response; }`: `api::Response` names the crate's
/// `Response` struct (documented as JSON), while the axum wrapper of the same
/// name, named through its external root, stays opaque.
#[rstest]
#[case::re_exported_crate_type("-> api::Response", Some(vec!["application/json"]))]
#[case::axum_response_stays_opaque("-> axum::response::Response", None)]
fn re_exported_crate_type_documents_its_body(
    #[case] ret: &str,
    #[case] content_types: Option<Vec<&str>>,
) {
    let known_schemas: HashSet<&str> = HashSet::from(["Response"]);
    let struct_definitions: HashMap<&str, &str> =
        HashMap::from([("Response", "pub struct Response { pub id: u32 }")]);
    let responses = parse_return_type(&return_type(ret), &known_schemas, &struct_definitions);
    let content = responses["200"]
        .content
        .as_ref()
        .map(|content| content.keys().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(content, content_types);
}
