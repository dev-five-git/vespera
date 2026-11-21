//! `#[route(responses = [...])]` emits a `vespera::Schema` assertion per
//! response type beside the handler. A generic handler's response type may
//! name its own type parameter, which is not in scope there, so that entry
//! alone is skipped; every other entry is still asserted (round-37 MR3-01,
//! round-38 MR3-01).

use quote::quote;
use rstest::rstest;

use super::*;

#[rstest]
#[case::concrete_handler(quote!(pub async fn get_one() -> String { String::new() }), true)]
#[case::generic_handler(quote!(pub async fn get_one<T>() -> String { String::new() }), false)]
fn response_type_assertions_skip_generic_handlers(
    #[case] item: proc_macro2::TokenStream,
    #[case] asserted: bool,
) {
    let output = process_route_attribute(quote!(get, responses = [(404, T)]), item)
        .unwrap()
        .to_string();
    assert_eq!(
        output.contains("__vespera_assert_schema"),
        asserted,
        "{output}"
    );
}

#[test]
fn generic_handler_still_asserts_its_concrete_response_types() {
    let item = quote!(
        pub async fn get_one<T>() -> String {
            String::new()
        }
    );
    let output =
        process_route_attribute(quote!(get, responses = [(404, NotFound), (400, T)]), item)
            .unwrap()
            .to_string();
    assert!(
        output.contains("__vespera_assert_schema :: < NotFound >"),
        "{output}"
    );
    assert!(
        !output.contains("__vespera_assert_schema :: < T >"),
        "{output}"
    );
}

#[test]
fn const_generic_handler_skips_only_entries_naming_its_const_param() {
    let item = quote!(
        pub async fn get_one<const N: usize>() -> String {
            String::new()
        }
    );
    let output =
        process_route_attribute(quote!(get, responses = [(400, N), (404, NotFound)]), item)
            .unwrap()
            .to_string();
    assert!(
        output.contains("__vespera_assert_schema :: < NotFound >"),
        "{output}"
    );
    assert!(
        !output.contains("__vespera_assert_schema :: < N >"),
        "{output}"
    );
}
