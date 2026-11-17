//! `#[route]` argument extractors: a malformed entry of `error_status` or
//! `responses` is a compile error, never a response the document silently
//! lacks.

use quote::quote;
use rstest::rstest;

use super::*;

#[test]
fn error_status_is_none_for_an_empty_array() {
    let arr: syn::ExprArray = syn::parse_quote!([]);
    assert_eq!(extract_error_status_codes(&arr).unwrap(), None);
}

#[test]
fn error_status_reads_every_literal_status() {
    let arr: syn::ExprArray = syn::parse_quote!([400, 404, 500]);
    assert_eq!(
        extract_error_status_codes(&arr).unwrap(),
        Some(vec![400, 404, 500])
    );
}

#[rstest]
#[case::named_constant(quote!([400, BAD_REQUEST]))]
#[case::string_status(quote!([400, "404"]))]
#[case::out_of_range_status(quote!([70_000]))]
fn error_status_rejects_a_malformed_entry(#[case] source: proc_macro2::TokenStream) {
    let array = syn::parse2::<syn::ExprArray>(source).expect("valid array fixture");
    let err = extract_error_status_codes(&array).expect_err("malformed entry must error");
    assert!(err.to_string().contains("error_status"), "{err}");
}

#[test]
fn string_literal_collector_keeps_only_string_entries() {
    // Only called after `require_string_literal_elems` has validated the array.
    let strings: syn::ExprArray = syn::parse_quote!(["users", admin, 7]);
    assert_eq!(
        collect_string_literal_values(&strings),
        vec!["users".to_string()]
    );
}

#[test]
fn typed_responses_are_none_for_an_empty_array() {
    let array: syn::ExprArray = syn::parse_quote!([]);
    assert_eq!(extract_typed_responses(&array).unwrap(), None);
}

#[test]
fn typed_responses_accept_a_qualified_schema_path() {
    let array: syn::ExprArray = syn::parse_quote!([(404, crate::errors::NotFound)]);
    assert_eq!(
        extract_typed_responses(&array).unwrap(),
        Some(vec![(404, "crate::errors::NotFound".to_string())])
    );
}

#[rstest]
#[case::not_a_tuple(quote!([NotFound]))]
#[case::missing_status(quote!([()]))]
#[case::non_integer_status(quote!([(NOT_FOUND, ErrorBody)]))]
#[case::out_of_range_status(quote!([(70_000, ErrorBody)]))]
#[case::missing_schema(quote!([(404,)]))]
#[case::non_path_schema(quote!([(404, "ErrorBody")]))]
fn typed_responses_reject_a_malformed_entry(#[case] source: proc_macro2::TokenStream) {
    let array = syn::parse2::<syn::ExprArray>(source).expect("valid array fixture");
    let err = extract_typed_responses(&array).expect_err("malformed entry must error");
    assert!(err.to_string().contains("responses"), "{err}");
}
