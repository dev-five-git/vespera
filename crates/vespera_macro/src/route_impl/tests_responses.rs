//! `#[route(responses = [...])]` emits a `vespera::Schema` assertion per
//! response type beside the handler. A generic handler's response types may
//! name its type parameters, which are not in scope there, so no assertion is
//! emitted for it (round-37 MR3-01).

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
