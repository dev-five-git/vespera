use std::collections::BTreeMap;

use rstest::rstest;
use vespera_core::schema::{OAuthFlow, OAuthFlows, SecurityScheme, SecuritySchemeType};

use super::*;

const OWNER: &str = "#[route] `security`";

fn parse(arr: &syn::ExprArray) -> syn::Result<Vec<SecurityRequirement>> {
    parse_security_requirements(arr, OWNER)
}

#[rstest]
#[case::names("names", syn::parse_quote!(["bearerAuth", "apiKey"]))]
#[case::empty_disables_security("empty_disables_security", syn::parse_quote!([]))]
#[case::scopes("scopes", syn::parse_quote!([("oauth2", ["read:users", "write:users"])]))]
#[case::empty_scopes("empty_scopes", syn::parse_quote!([("oauth2", [])]))]
#[case::and_group("and_group", syn::parse_quote!([["apiKey", ("oauth2", ["read:users"])]]))]
#[case::optional_auth("optional_auth", syn::parse_quote!([("oauth2", ["read:users"]), []]))]
fn requirements_parse(#[case] case: &str, #[case] arr: syn::ExprArray) {
    let parsed = parse(&arr).expect("valid security DSL");
    insta::assert_snapshot!(
        format!("security_requirements_{case}"),
        serde_json::to_string(&parsed).unwrap()
    );
}

#[rstest]
#[case::bare_ident("bare_ident", syn::parse_quote!([bearerAuth]))]
#[case::integer("integer", syn::parse_quote!([7]))]
#[case::one_tuple_field("one_tuple_field", syn::parse_quote!([("oauth2",)]))]
#[case::scopes_not_array("scopes_not_array", syn::parse_quote!([("oauth2", "read:users")]))]
#[case::name_not_string("name_not_string", syn::parse_quote!([(oauth2, ["read:users"])]))]
#[case::scope_not_string("scope_not_string", syn::parse_quote!([("oauth2", [read])]))]
#[case::duplicate_in_group("duplicate_in_group", syn::parse_quote!([["oauth2", ("oauth2", ["read"])]]))]
#[case::bad_entry_in_group("bad_entry_in_group", syn::parse_quote!([[1]]))]
fn malformed_requirements_are_rejected(#[case] case: &str, #[case] arr: syn::ExprArray) {
    let error = parse(&arr).expect_err("malformed security DSL must be rejected");
    insta::assert_snapshot!(
        format!("security_requirements_error_{case}"),
        error.to_string()
    );
}

fn scheme(r#type: SecuritySchemeType, flows: Option<OAuthFlows>) -> SecurityScheme {
    SecurityScheme {
        r#type,
        description: None,
        name: None,
        r#in: None,
        scheme: None,
        bearer_format: None,
        flows,
        open_id_connect_url: None,
    }
}

fn oauth2(scopes: &[&str]) -> SecurityScheme {
    scheme(
        SecuritySchemeType::OAuth2,
        Some(OAuthFlows {
            implicit: None,
            password: None,
            client_credentials: Some(OAuthFlow {
                authorization_url: None,
                token_url: Some("https://auth.example.com/token".to_string()),
                refresh_url: None,
                scopes: scopes
                    .iter()
                    .map(|scope| ((*scope).to_string(), String::new()))
                    .collect(),
            }),
            authorization_code: None,
        }),
    )
}

fn schemes() -> BTreeMap<String, SecurityScheme> {
    BTreeMap::from([
        ("oauth2".to_string(), oauth2(&["read:users", "write:users"])),
        ("empty".to_string(), oauth2(&[])),
        ("bare".to_string(), scheme(SecuritySchemeType::OAuth2, None)),
        (
            "oidc".to_string(),
            scheme(SecuritySchemeType::OpenIdConnect, None),
        ),
        ("bearer".to_string(), scheme(SecuritySchemeType::Http, None)),
    ])
}

fn requirement(entries: &[(&str, &[&str])]) -> SecurityRequirement {
    entries
        .iter()
        .map(|(name, scopes)| {
            (
                (*name).to_string(),
                scopes.iter().map(|scope| (*scope).to_string()).collect(),
            )
        })
        .collect()
}

#[rstest]
#[case::declared_scopes("oauth2", &["read:users", "write:users"], true)]
#[case::no_scopes("oauth2", &[], true)]
#[case::oidc_scopes_are_free_form("oidc", &["openid", "profile"], true)]
#[case::roles_on_http("bearer", &["admin"], true)]
#[case::oauth2_without_flows("bare", &["anything"], true)]
#[case::unknown_skipped_when_not_required("missing", &[], false)]
fn matching_requirements_pass(
    #[case] scheme: &str,
    #[case] scopes: &[&str],
    #[case] require: bool,
) {
    assert_eq!(
        check_requirements(&[requirement(&[(scheme, scopes)])], &schemes(), require),
        Ok(())
    );
}

#[rstest]
#[case::unknown_scheme("missing", &[], true)]
#[case::unknown_scope("oauth2", &["read:users", "delete:users"], true)]
#[case::unknown_scope_not_required("oauth2", &["admin"], false)]
#[case::scope_on_scopeless_scheme("empty", &["read"], true)]
fn mismatched_requirements_are_described(
    #[case] scheme: &str,
    #[case] scopes: &[&str],
    #[case] require: bool,
) {
    let schemes = schemes();
    let error = check_requirements(&[requirement(&[(scheme, scopes)])], &schemes, require)
        .expect_err("mismatch must be reported");
    let name = format!(
        "security_requirement_mismatch_{}_{require}",
        std::iter::once(scheme)
            .chain(scopes.iter().copied())
            .collect::<Vec<_>>()
            .join("_")
            .replace(':', "-")
    );
    insta::assert_snapshot!(
        name,
        describe_requirement_error(&error, "route `get_user` (`GET /users`)", &schemes)
    );
}

#[test]
fn unknown_scheme_without_any_declared_scheme_says_none() {
    let error = RequirementError::UnknownScheme("oauth2".to_string());
    assert_eq!(
        describe_requirement_error(&error, "top-level `security`", &BTreeMap::new()),
        "top-level `security` references unknown security scheme `oauth2`; declared in \
         `vespera!(security_schemes = [...])`: (none)"
    );
}
