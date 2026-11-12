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

/// A receiver argument names no extractor (route handlers are free
/// functions; `validate_route_fn` does not run here).
#[test]
fn receiver_argument_emits_nothing() {
    let sig: syn::Signature = syn::parse_quote!(fn method(&self) -> String);
    assert!(validated_payload_assertions(&sig).is_empty());
}
