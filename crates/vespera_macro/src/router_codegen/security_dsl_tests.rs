use rstest::rstest;
use syn::parse::Parser;

use super::*;
use crate::router_codegen::input::{AutoRouterInput, process_vespera_input};

fn flows(tokens: proc_macro2::TokenStream) -> syn::Result<OAuthFlows> {
    parse_flows.parse2(tokens)
}

fn vespera(tokens: proc_macro2::TokenStream) -> syn::Result<AutoRouterInput> {
    syn::parse2::<AutoRouterInput>(tokens)
}

#[test]
fn every_flow_kind_parses_with_its_urls_and_scopes() {
    let parsed = flows(quote::quote!(= {
        implicit = {
            authorization_url = "https://auth.example.com/authorize",
            scopes = { "read:users" = "Read users" },
        },
        password = { token_url = "https://auth.example.com/token", scopes = {} },
        client_credentials = {
            token_url = "https://auth.example.com/token",
            refresh_url = "https://auth.example.com/refresh",
            scopes = { "admin" = "Administer", "write:users" = "Modify users", },
        },
        authorization_code = {
            authorization_url = "https://auth.example.com/authorize",
            token_url = "https://auth.example.com/token",
        },
    }))
    .expect("valid flows");
    insta::assert_snapshot!(
        "security_dsl_every_flow_kind",
        serde_json::to_string_pretty(&parsed).unwrap()
    );
}

#[rstest]
#[case::empty("empty", quote::quote!(= {}))]
#[case::unknown_kind("unknown_kind", quote::quote!(= { device = { token_url = "https://t" } }))]
#[case::duplicate_kind(
    "duplicate_kind",
    quote::quote!(= { password = { token_url = "https://t" }, password = { token_url = "https://t" } })
)]
#[case::implicit_missing_authorization_url(
    "implicit_missing_authorization_url",
    quote::quote!(= { implicit = { token_url = "https://t" } })
)]
#[case::authorization_code_missing_token_url(
    "authorization_code_missing_token_url",
    quote::quote!(= { authorization_code = { authorization_url = "https://a" } })
)]
#[case::client_credentials_missing_token_url(
    "client_credentials_missing_token_url",
    quote::quote!(= { client_credentials = { scopes = {} } })
)]
#[case::unknown_field(
    "unknown_field",
    quote::quote!(= { password = { token_url = "https://t", audience = "x" } })
)]
#[case::duplicate_url(
    "duplicate_url",
    quote::quote!(= { password = { token_url = "https://t", token_url = "https://u" } })
)]
#[case::duplicate_scopes(
    "duplicate_scopes",
    quote::quote!(= { password = { token_url = "https://t", scopes = {}, scopes = {} } })
)]
#[case::duplicate_scope(
    "duplicate_scope",
    quote::quote!(= { password = { token_url = "https://t", scopes = { "a" = "x", "a" = "y" } } })
)]
fn malformed_flows_are_rejected(#[case] case: &str, #[case] tokens: proc_macro2::TokenStream) {
    let error = flows(tokens).expect_err("malformed flows must be rejected");
    insta::assert_snapshot!(
        format!("security_dsl_flows_error_{case}"),
        error.to_string()
    );
}

#[test]
fn oauth2_and_openid_connect_schemes_with_scoped_security() {
    let input = vespera(quote::quote!(
        security_schemes = [
            {
                name = "oauth2",
                type = "oauth2",
                description = "OAuth2 authorization server",
                flows = {
                    authorization_code = {
                        authorization_url = "https://auth.example.com/authorize",
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read users", "write:users" = "Modify users" },
                    },
                },
            },
            {
                name = "oidc",
                type = "openIdConnect",
                open_id_connect_url = "https://auth.example.com/.well-known/openid-configuration",
                flows = { password = { token_url = "https://t" } }
            }
        ],
    ));
    // `flows` is only meaningful on an `oauth2` scheme.
    insta::assert_snapshot!(
        "security_dsl_flows_on_openid_connect",
        input.err().expect("flows on openIdConnect").to_string()
    );

    let input = vespera(quote::quote!(
        security_schemes = [
            {
                flows = {
                    client_credentials = {
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read users" },
                    },
                },
                name = "oauth2",
                type = "oauth2"
            },
            {
                name = "oidc",
                type = "openIdConnect",
                open_id_connect_url = "https://auth.example.com/.well-known/openid-configuration",
            }
        ],
        security = [("oauth2", ["read:users"]), ("oidc", ["openid", "profile"])],
    ))
    .expect("valid oauth2 + openIdConnect schemes");
    let processed = process_vespera_input(input);
    insta::assert_snapshot!(
        "security_dsl_oauth2_and_openid_connect",
        serde_json::to_string_pretty(&serde_json::json!({
            "securitySchemes": processed.security_schemes,
            "security": processed.security,
        }))
        .unwrap()
    );
}

#[rstest]
#[case::oauth2_without_flows(
    "oauth2_without_flows",
    quote::quote!(security_schemes = [{ name = "oauth2", type = "oauth2" }])
)]
#[case::undeclared_scope(
    "undeclared_scope",
    quote::quote!(
        security_schemes = [{
            name = "oauth2",
            type = "oauth2",
            flows = { client_credentials = { token_url = "https://t", scopes = { "read" = "Read" } } },
        }],
        security = [("oauth2", ["read", "admin"])],
    )
)]
#[case::malformed_requirement("malformed_requirement", quote::quote!(security = [oauth2]))]
fn invalid_vespera_security_is_rejected(
    #[case] case: &str,
    #[case] tokens: proc_macro2::TokenStream,
) {
    let error = vespera(tokens)
        .err()
        .expect("invalid security config must be rejected");
    insta::assert_snapshot!(
        format!("security_dsl_vespera_error_{case}"),
        error.to_string()
    );
}

#[test]
fn top_level_scopes_of_undeclared_schemes_wait_for_merged_children() {
    // `oauth2` may come from a `merge = [...]` child: not checked at parse time.
    let input = vespera(quote::quote!(security = [("oauth2", ["admin"])]))
        .expect("an undeclared scheme is checked after merging");
    assert_eq!(
        input.security.expect("security parsed")[0]["oauth2"],
        ["admin"]
    );
}
