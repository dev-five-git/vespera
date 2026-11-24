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

/// A rejected `responses` entry is an error before the route is stored
/// (round-40 MR3-L3): the IDE view must not keep a `$ref` to the parameter.
/// The message wraps across lines with `\` continuations, not runs of spaces.
#[test]
fn rejected_generic_responses_entry_is_not_stored() {
    let item = quote!(
        pub async fn get_rejected_generic_storage<T>() -> String {
            String::new()
        }
    );
    let error = process_route_attribute(quote!(get, responses = [(404, T)]), item).unwrap_err();
    assert!(error.to_string().contains("dangling `$ref`"), "{error}");
    assert!(!error.to_string().contains("          "), "{error}");
    assert!(
        !current_crate_routes()
            .iter()
            .any(|stored| stored.fn_name == "get_rejected_generic_storage"),
        "a rejected route must not be stored"
    );
}

/// A lifetime named like a module does not make `a::NotFound` a generic
/// reference: only a type parameter can head a path (round-41 R41-MR3-L2).
#[test]
fn lifetime_named_like_a_module_does_not_reject_a_responses_path() {
    let item = quote!(
        pub async fn get_one<'a>() -> String {
            String::new()
        }
    );
    let output = process_route_attribute(quote!(get, responses = [(404, a::NotFound)]), item)
        .unwrap()
        .to_string();
    assert!(
        output.contains("__vespera_assert_schema :: < a :: NotFound >"),
        "{output}"
    );
}

/// The head of `T::Assoc` names a type parameter, so it is still rejected.
#[test]
fn type_param_head_of_a_responses_path_is_rejected() {
    let item = quote!(
        pub async fn get_one<T: Clone>() -> String {
            String::new()
        }
    );
    let error =
        process_route_attribute(quote!(get, responses = [(404, T::Assoc)]), item).unwrap_err();
    assert!(error.to_string().contains("dangling `$ref`"), "{error}");
}
