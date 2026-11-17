//! Security schemes and operation metadata.
//!
//! `vespera!(security_schemes = [...])` in `lib.rs` declares `bearerAuth`
//! (HTTP bearer, JWT), `apiKey` (`X-API-Key` header), `oauth2` (authorization
//! code + client credentials flows with scopes) and `oidc` (OpenID Connect)
//! under `components.securitySchemes`; each route here opts in with
//! `#[route(security = [...])]` — `("oauth2", ["read:users"])` also names the
//! required scopes, checked against the flows at compile time.
//! `security = []` documents an explicitly unauthenticated operation.
//!
//! The credential checks below are demo code, not production auth: a real
//! service verifies signed tokens and stored keys, and only compares secrets
//! in constant time (`credential_matches`).
//!
//! The scheme only *documents* auth — enforcing it stays the handler's (or a
//! middleware's) job, shown here with a plain `HeaderMap` check. A `401`
//! carries the `WWW-Authenticate` challenge RFC 9110 requires, and a
//! missing scope answers `403` with RFC 6750's `insufficient_scope` error.
//!
//! The routes also show the per-operation metadata attributes:
//! `operation_id`, `summary`, `description`, `response_example`,
//! `deprecated` and `error_status`.

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{
        Json,
        http::{
            HeaderMap, HeaderName, StatusCode,
            header::{AUTHORIZATION, WWW_AUTHENTICATE},
        },
    },
};

/// An auth failure: the status plus its `WWW-Authenticate` challenge.
pub type AuthError = (StatusCode, [(HeaderName, &'static str); 1]);

/// `401` challenging the caller for `scheme` credentials.
const fn unauthorized(scheme: &'static str) -> AuthError {
    (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, scheme)])
}

/// The credential of an `Authorization: Bearer <token>` header. The
/// auth-scheme name is case-insensitive (RFC 9110 section 11.1), so `bearer`
/// is accepted too.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

/// Whether the presented credential is `expected`, compared in constant time
/// (every byte is folded whatever the first mismatch), so the response time
/// does not reveal how long a matching prefix is. The length check leaks only
/// the length, which these demo credentials make public anyway.
fn credential_matches(presented: Option<&str>, expected: &str) -> bool {
    let Some(presented) = presented else {
        return false;
    };
    let (presented, expected) = (presented.as_bytes(), expected.as_bytes());
    presented.len() == expected.len()
        && presented
            .iter()
            .zip(expected)
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// The authenticated caller.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct CurrentUser {
    pub id: u32,
    pub name: String,
}

/// Token the demo accepts (`Authorization: Bearer demo-token`).
pub const DEMO_TOKEN: &str = "demo-token";
/// API key the demo accepts (`X-API-Key: demo-key`).
pub const DEMO_API_KEY: &str = "demo-key";

/// Return the caller identified by the bearer token.
#[vespera::route(
    get,
    path = "/me",
    tags = ["secured"],
    security = ["bearerAuth"],
    operation_id = "getCurrentUser",
    summary = "Current user",
    description = "Requires `Authorization: Bearer <token>`; answers 401 with a `WWW-Authenticate: Bearer` challenge otherwise.",
    response_example = r#"{"id": 1, "name": "demo"}"#,
    error_status = [401]
)]
pub async fn current_user(headers: HeaderMap) -> Result<Json<CurrentUser>, AuthError> {
    if !credential_matches(bearer_token(&headers), DEMO_TOKEN) {
        return Err(unauthorized("Bearer"));
    }
    Ok(Json(CurrentUser {
        id: 1,
        name: "demo".to_string(),
    }))
}

/// Legacy report listing, guarded by an API key.
///
/// Marked `deprecated` — Swagger UI renders it struck through.
// It also carries Rust's `#[deprecated]`: `vespera!` mounting it must not
// warn (the example builds under `-D warnings`).
#[deprecated = "legacy listing, kept for old clients"]
#[vespera::route(
    get,
    path = "/reports",
    tags = ["secured"],
    security = ["apiKey"],
    deprecated,
    error_status = [401]
)]
pub async fn legacy_reports(headers: HeaderMap) -> Result<Json<Vec<String>>, AuthError> {
    let key = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    if !credential_matches(key, DEMO_API_KEY) {
        return Err(unauthorized("ApiKey"));
    }
    Ok(Json(vec!["2026-q1".to_string(), "2026-q2".to_string()]))
}

/// Public status — `security = []` marks it explicitly unauthenticated.
#[vespera::route(get, path = "/public", tags = ["secured"], security = [])]
pub async fn public_status() -> &'static str {
    "public"
}

/// Token the demo maps to the `read:users` OAuth2 scope
/// (`Authorization: Bearer read-users-token`).
pub const READ_USERS_TOKEN: &str = "read-users-token";

/// User directory — needs the `read:users` scope, granted either by the
/// OAuth2 server or by the OpenID Connect provider (alternatives, OR).
///
/// A scope missing from every flow of `oauth2` is a compile error.
#[vespera::route(
    get,
    path = "/directory",
    tags = ["secured"],
    security = [("oauth2", ["read:users"]), ("oidc", ["openid", "read:users"])],
    error_status = [401, 403]
)]
pub async fn user_directory(headers: HeaderMap) -> Result<Json<Vec<String>>, AuthError> {
    match bearer_token(&headers) {
        Some(READ_USERS_TOKEN) => Ok(Json(vec!["alice".to_string(), "bob".to_string()])),
        // Authenticated, but the token lacks the `read:users` scope.
        Some(DEMO_TOKEN) => Err((
            StatusCode::FORBIDDEN,
            [(
                WWW_AUTHENTICATE,
                r#"Bearer error="insufficient_scope", scope="read:users""#,
            )],
        )),
        _ => Err(unauthorized("Bearer")),
    }
}

#[cfg(test)]
mod tests {
    use super::credential_matches;

    #[test]
    fn credential_matches_only_the_exact_secret() {
        assert!(credential_matches(Some("demo-key"), "demo-key"));
        assert!(!credential_matches(None, "demo-key"));
        assert!(!credential_matches(Some("demo-ke"), "demo-key"));
        assert!(!credential_matches(Some("demo-keyx"), "demo-key"));
        assert!(!credential_matches(Some("demo-kex"), "demo-key"));
    }
}
