//! `#[route]` emits a `vespera::__validation::AssertValidate` assertion for the
//! payload of every `Validated` / `ValidatedWith` argument, so a payload without
//! a `garde::Validate` impl fails with vespera's own diagnostic at the argument.

use quote::quote;
use rstest::rstest;

use super::*;

#[rstest]
#[case::path_then_validated_json(
    "path_then_validated_json",
    quote!(
        pub async fn update(
            Path(id): Path<i64>,
            Validated(Json(body)): Validated<Json<UpdateUser>>,
        ) -> String {
            String::new()
        }
    )
)]
#[case::validated_with_query_and_form(
    "validated_with_query_and_form",
    quote!(
        pub async fn search(
            filter: vespera::ValidatedWith<Limits, Query<Filter>>,
            Validated(Form(form)): Validated<axum::Form<Login>>,
        ) -> String {
            String::new()
        }
    )
)]
#[case::custom_extractor_and_plain_json(
    "custom_extractor_and_plain_json",
    quote!(
        pub async fn create(
            Validated(custom): Validated<MyExtractor>,
            Json(body): Json<CreateUser>,
        ) -> String {
            String::new()
        }
    )
)]
#[case::result_validated_json(
    "result_validated_json",
    quote!(
        pub async fn create(
            body: std::result::Result<Validated<Json<CreateUser>>, ValidationRejection>,
        ) -> String {
            String::new()
        }
    )
)]
#[case::generic_handler_skipped(
    "generic_handler_skipped",
    quote!(
        pub async fn generic<T>(Validated(Json(body)): Validated<Json<T>>) -> String {
            String::new()
        }
    )
)]
fn route_emits_validated_payload_assertions(
    #[case] name: &str,
    #[case] item: proc_macro2::TokenStream,
) {
    let output = process_route_attribute(quote!(post), item).unwrap();
    let file: syn::File = syn::parse2(output).unwrap();
    insta::assert_snapshot!(
        format!("route_validated_payload_assertions_{name}"),
        prettyplease::unparse(&file)
    );
}

/// A generic handler skips only the payload naming its own type parameter; a
/// concrete payload beside it is still asserted (round-38 MR3-02).
#[test]
fn generic_handler_still_asserts_its_concrete_payloads() {
    let sig: syn::Signature = syn::parse_quote!(
        fn generic<T>(
            Validated(Json(first)): Validated<Json<T>>,
            Validated(Json(second)): Validated<Json<Other>>,
        ) -> String
    );
    let output = validated_payload_assertions(&sig).to_string();
    assert!(
        output.contains("__vespera_assert_validate :: < Other >"),
        "{output}"
    );
    assert!(
        !output.contains("__vespera_assert_validate :: < T >"),
        "{output}"
    );
}

/// A receiver argument names no extractor (route handlers are free
/// functions; `validate_route_fn` does not run here).
#[test]
fn receiver_argument_emits_nothing() {
    let sig: syn::Signature = syn::parse_quote!(fn method(&self) -> String);
    assert!(validated_payload_assertions(&sig).is_empty());
}

/// A rejected `#[route]` keeps the handler next to its error, so the
/// `vespera!` that mounts it reports nothing else.
#[test]
fn rejected_route_keeps_the_item() {
    let item = quote!(
        async fn private_handler() -> String {
            String::new()
        }
    );
    let output = route_attribute(quote!(get), item).to_string();
    assert!(output.starts_with(":: core :: compile_error !"), "{output}");
    assert!(output.contains("async fn private_handler ()"), "{output}");
}
