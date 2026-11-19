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
#[case::zero_status(quote!([0]))]
#[case::below_range_status(quote!([99]))]
#[case::above_range_status(quote!([600]))]
#[case::unknown_status_999(quote!([999]))]
fn error_status_rejects_a_malformed_entry(#[case] source: proc_macro2::TokenStream) {
    let array = syn::parse2::<syn::ExprArray>(source).expect("valid array fixture");
    let err = extract_error_status_codes(&array).expect_err("malformed entry must error");
    assert!(err.to_string().contains("error_status"), "{err}");
}

#[test]
fn error_status_accepts_the_range_bounds() {
    let arr: syn::ExprArray = syn::parse_quote!([100, 599]);
    assert_eq!(
        extract_error_status_codes(&arr).unwrap(),
        Some(vec![100, 599])
    );
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
#[case::status_below_range(quote!([(99, ErrorBody)]))]
#[case::status_above_range(quote!([(600, ErrorBody)]))]
#[case::missing_schema(quote!([(404,)]))]
#[case::non_path_schema(quote!([(404, "ErrorBody")]))]
#[case::extra_element_is_not_dropped(quote!([(404, ErrorBody, "not found")]))]
fn typed_responses_reject_a_malformed_entry(#[case] source: proc_macro2::TokenStream) {
    let array = syn::parse2::<syn::ExprArray>(source).expect("valid array fixture");
    let err = extract_typed_responses(&array).expect_err("malformed entry must error");
    assert!(err.to_string().contains("responses"), "{err}");
}

#[test]
fn typed_responses_reject_a_repeated_status() {
    let array: syn::ExprArray = syn::parse_quote!([(404, NotFound), (500, Boom), (404, Other)]);
    let err = extract_typed_responses(&array).expect_err("repeated status must error");
    assert!(err.to_string().contains("more than once"), "{err}");
    assert!(err.to_string().contains("404"), "{err}");
}
