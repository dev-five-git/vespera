//! `vespera!` security DSL pieces beyond the flat scheme fields: the
//! top-level `security = [...]` requirement list and the OAuth2 `flows = {...}`
//! block of a `security_schemes` entry.
//!
//! ```text
//! security_schemes = [{
//!     name = "oauth2", type = "oauth2",
//!     flows = {
//!         authorization_code = {
//!             authorization_url = "https://auth.example.com/authorize",
//!             token_url = "https://auth.example.com/token",
//!             refresh_url = "https://auth.example.com/refresh",   // optional
//!             scopes = { "read:users" = "Read users", "write:users" = "Modify users" },
//!         },
//!         client_credentials = { token_url = "...", scopes = { ... } },
//!     },
//! }],
//! security = [("oauth2", ["read:users"])],
//! ```

use std::collections::{BTreeMap, HashSet};

use syn::{LitStr, parse::ParseStream, spanned::Spanned};
use vespera_core::schema::{OAuthFlow, OAuthFlows, SecurityRequirement};

use super::input::SecuritySchemeConfig;
use crate::security::{
    check_requirements, describe_requirement_error, parse_security_requirements,
};

/// Owner label of the top-level requirement list in diagnostics.
const TOP_LEVEL: &str = "vespera! `security`";

/// Parse `security = [...]` (after the `security` ident) into requirements,
/// keeping the array for spanned follow-up diagnostics.
pub fn parse_security_values(
    input: ParseStream,
) -> syn::Result<(Vec<SecurityRequirement>, syn::ExprArray)> {
    input.parse::<syn::Token![=]>()?;
    let arr: syn::ExprArray = input.parse()?;
    Ok((parse_security_requirements(&arr, TOP_LEVEL)?, arr))
}

/// Reject a top-level requirement asking a locally declared `oauth2` scheme
/// for a scope none of its flows declares — spanned at the `security` array.
///
/// Scheme EXISTENCE is not checked here: a `merge = [...]` child may still
/// contribute the scheme; that check runs after merging, at spec generation.
pub fn validate_top_level_scopes(
    requirements: &[SecurityRequirement],
    arr: &syn::ExprArray,
    schemes: Option<&[SecuritySchemeConfig]>,
) -> syn::Result<()> {
    let schemes: BTreeMap<String, _> = schemes
        .unwrap_or_default()
        .iter()
        .map(|config| (config.name.clone(), config.scheme.clone()))
        .collect();
    check_requirements(requirements, &schemes, false).map_err(|error| {
        syn::Error::new(
            arr.span(),
            describe_requirement_error(&error, "top-level `security`", &schemes),
        )
    })
}

/// The four OAuth2 flow kinds, by DSL name.
const FLOW_KINDS: [&str; 4] = [
    "implicit",
    "password",
    "client_credentials",
    "authorization_code",
];

/// Parse `= { <flow> = { ... }, ... }` (after the `flows` field name).
pub fn parse_flows(input: ParseStream) -> syn::Result<OAuthFlows> {
    input.parse::<syn::Token![=]>()?;
    let content;
    let brace = syn::braced!(content in input);
    let mut flows = OAuthFlows {
        implicit: None,
        password: None,
        client_credentials: None,
        authorization_code: None,
    };
    let mut seen = HashSet::new();
    while !content.is_empty() {
        let kind: syn::Ident = content.parse()?;
        let kind_str = kind.to_string();
        let slot = match kind_str.as_str() {
            "implicit" => &mut flows.implicit,
            "password" => &mut flows.password,
            "client_credentials" => &mut flows.client_credentials,
            "authorization_code" => &mut flows.authorization_code,
            _ => {
                return Err(syn::Error::new(
                    kind.span(),
                    format!(
                        "unknown OAuth2 flow: `{kind_str}`. Expected {}",
                        quoted_list(&FLOW_KINDS)
                    ),
                ));
            }
        };
        if !seen.insert(kind_str.clone()) {
            return Err(syn::Error::new(
                kind.span(),
                format!("duplicate OAuth2 flow: `{kind_str}`"),
            ));
        }
        content.parse::<syn::Token![=]>()?;
        *slot = Some(parse_flow(&content, &kind)?);
        if !content.peek(syn::Token![,]) {
            break;
        }
        content.parse::<syn::Token![,]>()?;
    }
    if seen.is_empty() {
        return Err(syn::Error::new(
            brace.span.join(),
            format!(
                "`flows` must configure at least one OAuth2 flow: {}",
                quoted_list(&FLOW_KINDS)
            ),
        ));
    }
    Ok(flows)
}

/// Parse one `{ authorization_url = ..., token_url = ..., refresh_url = ..., scopes = {...} }`
/// and check the URLs OpenAPI requires for the flow `kind`.
fn parse_flow(input: ParseStream, kind: &syn::Ident) -> syn::Result<OAuthFlow> {
    let content;
    syn::braced!(content in input);
    let mut urls: BTreeMap<String, String> = BTreeMap::new();
    let mut scopes = None;
    while !content.is_empty() {
        let field: syn::Ident = content.parse()?;
        let field_str = field.to_string();
        if urls.contains_key(&field_str) || (field_str == "scopes" && scopes.is_some()) {
            return Err(syn::Error::new(
                field.span(),
                format!("duplicate OAuth2 flow field: `{field_str}`"),
            ));
        }
        content.parse::<syn::Token![=]>()?;
        match field_str.as_str() {
            "authorization_url" | "token_url" | "refresh_url" => {
                urls.insert(field_str, content.parse::<LitStr>()?.value());
            }
            "scopes" => scopes = Some(parse_scopes(&content)?),
            _ => {
                return Err(syn::Error::new(
                    field.span(),
                    format!(
                        "unknown OAuth2 flow field: `{field_str}`. Expected `authorization_url`, \
                         `token_url`, `refresh_url`, or `scopes`"
                    ),
                ));
            }
        }
        if !content.peek(syn::Token![,]) {
            break;
        }
        content.parse::<syn::Token![,]>()?;
    }
    let required: &[&str] = match kind.to_string().as_str() {
        "implicit" => &["authorization_url"],
        "authorization_code" => &["authorization_url", "token_url"],
        _ => &["token_url"],
    };
    if let Some(missing) = required.iter().find(|field| !urls.contains_key(**field)) {
        return Err(syn::Error::new(
            kind.span(),
            format!("OAuth2 `{kind}` flow is missing required field `{missing}`"),
        ));
    }
    Ok(OAuthFlow {
        authorization_url: urls.remove("authorization_url"),
        token_url: urls.remove("token_url"),
        refresh_url: urls.remove("refresh_url"),
        scopes: scopes.unwrap_or_default(),
    })
}

/// Parse `{ "scope" = "description", ... }`.
fn parse_scopes(input: ParseStream) -> syn::Result<BTreeMap<String, String>> {
    let content;
    syn::braced!(content in input);
    let mut scopes = BTreeMap::new();
    while !content.is_empty() {
        let scope: LitStr = content.parse()?;
        content.parse::<syn::Token![=]>()?;
        let description: LitStr = content.parse()?;
        if scopes.insert(scope.value(), description.value()).is_some() {
            return Err(syn::Error::new(
                scope.span(),
                format!("duplicate OAuth2 scope: `{}`", scope.value()),
            ));
        }
        if !content.peek(syn::Token![,]) {
            break;
        }
        content.parse::<syn::Token![,]>()?;
    }
    Ok(scopes)
}

fn quoted_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "security_dsl_tests.rs"]
mod tests;
