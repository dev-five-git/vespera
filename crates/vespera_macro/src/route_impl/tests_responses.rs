//! `#[route(responses = [...])]` emits a `vespera::Schema` assertion per
//! response type beside the handler. A generic handler's response type may
//! not name its own type or const parameter (no schema exists for it outside
//! an instantiation): that is a spanned error. Its other types are asserted
//! (round-37 MR3-01, round-38 MR3-01, round-39 MR3-L2).

use quote::quote;
use rstest::rstest;

use super::*;

#[rstest]
#[case::concrete_handler(quote!(pub async fn get_one() -> String { String::new() }), false)]
#[case::generic_handler(quote!(pub async fn get_one<T>() -> String { String::new() }), true)]
fn response_type_naming_a_handler_param_is_rejected(
    #[case] item: proc_macro2::TokenStream,
    #[case] rejected: bool,
) {
    match process_route_attribute(quote!(get, responses = [(404, T)]), item) {
        Ok(output) => {
            assert!(!rejected, "{output}");
            assert!(
                output.to_string().contains("__vespera_assert_schema"),
                "{output}"
            );
        }
        Err(error) => {
            assert!(rejected, "{error}");
            assert!(error.to_string().contains("dangling `$ref`"), "{error}");
        }
    }
}

#[test]
fn generic_handler_still_asserts_its_concrete_response_types() {
    let item = quote!(
        pub async fn get_one<T>() -> String {
            String::new()
        }
    );
    let output = process_route_attribute(
        quote!(get, responses = [(404, NotFound), (400, Conflict)]),
        item,
    )
    .unwrap()
    .to_string();
    assert!(
        output.contains("__vespera_assert_schema :: < NotFound >"),
        "{output}"
    );
    assert!(
        output.contains("__vespera_assert_schema :: < Conflict >"),
        "{output}"
    );
}

#[test]
fn generic_handler_asserts_a_qualified_response_type_named_like_its_param() {
    let item = quote!(
        pub async fn get_one<Error>() -> String {
            String::new()
        }
    );
    let output = process_route_attribute(quote!(get, responses = [(404, api::Error)]), item)
        .unwrap()
        .to_string();
    assert!(
        output.contains("__vespera_assert_schema :: < api :: Error >"),
        "{output}"
    );
}

#[test]
fn const_generic_handler_rejects_an_entry_naming_its_const_param() {
    let item = quote!(
        pub async fn get_one<const N: usize>() -> String {
            String::new()
        }
    );
    let error = process_route_attribute(quote!(get, responses = [(400, N)]), item).unwrap_err();
    assert!(error.to_string().contains("dangling `$ref`"), "{error}");
}
