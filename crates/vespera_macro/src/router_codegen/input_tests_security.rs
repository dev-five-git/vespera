//! Malformed `security_schemes` / `tags` nested configuration: each rejection
//! names the specific field. Split out of `input_tests.rs` to keep that file
//! under the 1000-line cap.
use super::*;
use rstest::rstest;

#[rstest]
#[case::unknown_tag_field(
    quote::quote!(tags = [{ name = "users", color = "blue" }]),
    "unknown tag field: `color`. Expected `name` or `description`"
)]
#[case::unknown_security_field(
    quote::quote!(security_schemes = [{ name = "auth", type = "mutualTLS", audience = "api" }]),
    "unknown security scheme field: `audience`. Expected `name`, `type`, `description`, `header_name`, `in`, `scheme`, `bearer_format`, `open_id_connect_url`, or `flows`"
)]
#[case::security_missing_name(
    quote::quote!(security_schemes = [{ type = "mutualTLS" }]),
    "vespera! macro: security scheme missing required `name` field."
)]
#[case::security_missing_type(
    quote::quote!(security_schemes = [{ name = "auth" }]),
    "vespera! macro: security scheme missing required `type` field."
)]
#[case::api_key_missing_header_name(
    quote::quote!(security_schemes = [{ name = "apiKey", type = "apiKey", in = "header" }]),
    "vespera! macro: security scheme `apiKey` of type `apiKey` is missing required field `header_name` (the api-key parameter name)"
)]
#[case::api_key_empty_header_name(
    quote::quote!(security_schemes = [{ name = "k", type = "apiKey", header_name = "", in = "header" }]),
    "vespera! macro: `header_name` must not be empty"
)]
#[case::http_empty_scheme(
    quote::quote!(security_schemes = [{ name = "b", type = "http", scheme = "" }]),
    "vespera! macro: `scheme` must not be empty"
)]
#[case::http_empty_bearer_format(
    quote::quote!(security_schemes = [{ name = "b", type = "http", scheme = "bearer", bearer_format = "" }]),
    "vespera! macro: `bearer_format` must not be empty"
)]
#[case::api_key_with_http_scheme(
    quote::quote!(security_schemes = [{ name = "key", type = "apiKey", header_name = "X-Key", in = "header", scheme = "bearer" }]),
    "vespera! macro: security scheme `key` of type `apiKey` cannot declare `scheme`; only `http` schemes have scheme"
)]
#[case::http_with_header_name(
    quote::quote!(security_schemes = [{ name = "basic", type = "http", scheme = "basic", header_name = "X" }]),
    "vespera! macro: security scheme `basic` of type `http` cannot declare `header_name`; only `apiKey` schemes have header_name"
)]
#[case::oauth2_with_oidc_url(
    quote::quote!(security_schemes = [{ name = "o", type = "oauth2", open_id_connect_url = "https://x/.well-known", flows = { client_credentials = { token_url = "https://t", scopes = {} } } }]),
    "vespera! macro: security scheme `o` of type `oauth2` cannot declare `open_id_connect_url`; only `openIdConnect` schemes have open_id_connect_url"
)]
#[case::bearer_format_on_basic(
    quote::quote!(security_schemes = [{ name = "basic", type = "http", scheme = "basic", bearer_format = "JWT" }]),
    "vespera! macro: security scheme `basic` declares `bearer_format` with `scheme = \"basic\"`; OpenAPI defines `bearerFormat` only for `scheme = \"bearer\"`"
)]
#[case::empty_token_url(
    quote::quote!(security_schemes = [{ name = "o", type = "oauth2", flows = { client_credentials = { token_url = "", scopes = {} } } }]),
    "vespera! macro: `token_url` must be an absolute `http://` or `https://` URL, got \"\""
)]
#[case::relative_oidc_url(
    quote::quote!(security_schemes = [{ name = "oidc", type = "openIdConnect", open_id_connect_url = "/.well-known/openid-configuration" }]),
    "vespera! macro: `open_id_connect_url` must be an absolute `http://` or `https://` URL, got \"/.well-known/openid-configuration\""
)]
#[case::invalid_security_type(
    quote::quote!(security_schemes = [{ name = "auth", type = "custom" }]),
    "invalid security scheme type: `custom`. Expected `apiKey`, `http`, `mutualTLS`, `oauth2`, or `openIdConnect`"
)]
fn malformed_nested_configuration_reports_the_specific_field_error(
    #[case] tokens: proc_macro2::TokenStream,
    #[case] expected: &str,
) {
    let error = syn::parse2::<AutoRouterInput>(tokens)
        .err()
        .expect("fixture must be rejected");

    assert_eq!(error.to_string(), expected);
}
