//! OpenAPI security requirements shared by `#[route(security = [...])]` and
//! `vespera!(security = [...])`: the requirement DSL and the scheme / scope
//! reference checks.
//!
//! DSL (each array element is one alternative — OpenAPI ORs them):
//!
//! - `"bearerAuth"` → `{ "bearerAuth": [] }`
//! - `("oauth2", ["read:users", "write:users"])` → `{ "oauth2": ["read:users", "write:users"] }`
//! - `["apiKey", ("oauth2", ["read:users"])]` → `{ "apiKey": [], "oauth2": ["read:users"] }`
//!   (AND group — every scheme of the group is required together)
//! - `[]` → `{}` (anonymous alternative: authentication becomes optional)

use std::collections::BTreeMap;

use vespera_core::schema::{SecurityRequirement, SecurityScheme, SecuritySchemeType};

/// Parse a `security = [...]` array into OpenAPI Security Requirement Objects.
///
/// `owner` names the attribute in diagnostics (e.g. ``#[route] `security` ``).
pub fn parse_security_requirements(
    arr: &syn::ExprArray,
    owner: &str,
) -> syn::Result<Vec<SecurityRequirement>> {
    arr.elems
        .iter()
        .map(|elem| {
            let mut requirement = SecurityRequirement::new();
            if let syn::Expr::Array(group) = elem {
                for entry in &group.elems {
                    let (name, scopes) = parse_entry(entry, owner)?;
                    if requirement.insert(name.clone(), scopes).is_some() {
                        return Err(syn::Error::new_spanned(
                            entry,
                            format!("{owner} lists scheme `{name}` twice in one AND group"),
                        ));
                    }
                }
            } else {
                let (name, scopes) = parse_entry(elem, owner)?;
                requirement.insert(name, scopes);
            }
            Ok(requirement)
        })
        .collect()
}

/// One `"scheme"` or `("scheme", ["scope", ...])` entry.
fn parse_entry(expr: &syn::Expr, owner: &str) -> syn::Result<(String, Vec<String>)> {
    if let Some(name) = string_lit(expr) {
        return Ok((name, Vec::new()));
    }
    if let syn::Expr::Tuple(tuple) = expr
        && tuple.elems.len() == 2
        && let Some(name) = string_lit(&tuple.elems[0])
        && let syn::Expr::Array(scopes) = &tuple.elems[1]
    {
        let scopes = scopes
            .elems
            .iter()
            .map(|scope| {
                string_lit(scope).ok_or_else(|| {
                    syn::Error::new_spanned(
                        scope,
                        format!("{owner} scopes must be string literals, e.g. `[\"read:users\"]`"),
                    )
                })
            })
            .collect::<syn::Result<Vec<_>>>()?;
        return Ok((name, scopes));
    }
    Err(syn::Error::new_spanned(
        expr,
        format!(
            "{owner} entries must be a scheme name (`\"bearerAuth\"`), a scheme with scopes \
             (`(\"oauth2\", [\"read:users\"])`), or an AND group of those \
             (`[\"apiKey\", (\"oauth2\", [\"read:users\"])]`)"
        ),
    ))
}

fn string_lit(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(lit),
        ..
    }) = expr
    {
        Some(lit.value())
    } else {
        None
    }
}

/// Why a security requirement does not match the declared schemes.
#[derive(Debug, PartialEq, Eq)]
pub enum RequirementError {
    /// The requirement names a scheme absent from `components.securitySchemes`.
    UnknownScheme(String),
    /// The requirement asks an `oauth2` scheme for a scope none of its flows declares.
    UnknownScope {
        scheme: String,
        scope: String,
        /// Every scope the scheme's flows declare (sorted), for the diagnostic.
        declared: Vec<String>,
    },
}

/// Check `requirements` against `schemes`.
///
/// With `require_declared`, a scheme missing from `schemes` is an error;
/// without it (the `vespera!` parse-time check, where a merged child app may
/// still contribute the scheme) only declared schemes are checked.
///
/// Scopes are checked for `oauth2` schemes only: OpenID Connect scopes live in
/// the discovery document (unknown at compile time) and other scheme types
/// carry free-form role names (OpenAPI 3.1 §4.8.30).
pub fn check_requirements(
    requirements: &[SecurityRequirement],
    schemes: &BTreeMap<String, SecurityScheme>,
    require_declared: bool,
) -> Result<(), RequirementError> {
    for (name, scopes) in requirements.iter().flatten() {
        let Some(scheme) = schemes.get(name) else {
            if require_declared {
                return Err(RequirementError::UnknownScheme(name.clone()));
            }
            continue;
        };
        if scheme.r#type != SecuritySchemeType::OAuth2 {
            continue;
        }
        let Some(flows) = scheme.flows.as_ref() else {
            continue;
        };
        if let Some(scope) = scopes.iter().find(|scope| !flows.declares_scope(scope)) {
            return Err(RequirementError::UnknownScope {
                scheme: name.clone(),
                scope: scope.clone(),
                declared: flows.scope_names().into_iter().map(String::from).collect(),
            });
        }
    }
    Ok(())
}

/// Human-readable diagnostic for a [`RequirementError`] raised by `user`
/// (e.g. ``route `get_user` (`GET /users/{id}`)``).
pub fn describe_requirement_error(
    error: &RequirementError,
    user: &str,
    schemes: &BTreeMap<String, SecurityScheme>,
) -> String {
    let quoted = |names: Vec<&str>| {
        if names.is_empty() {
            "(none)".to_string()
        } else {
            names
                .into_iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    match error {
        RequirementError::UnknownScheme(name) => format!(
            "{user} references unknown security scheme `{name}`; declared in \
             `vespera!(security_schemes = [...])`: {}",
            quoted(schemes.keys().map(String::as_str).collect())
        ),
        RequirementError::UnknownScope {
            scheme,
            scope,
            declared,
        } => format!(
            "{user} requires scope `{scope}`, which no flow of oauth2 security scheme \
             `{scheme}` declares; declared scopes: {}",
            quoted(declared.iter().map(String::as_str).collect())
        ),
    }
}

#[cfg(test)]
mod tests;
