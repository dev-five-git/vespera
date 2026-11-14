//! The authenticated caller a trusted host attached to a request.
//!
//! An embedding host (the Java bridge's `VesperaProxyController`, a C FFI
//! caller, …) that has already authenticated the request — e.g. Spring
//! Security — passes the result in the optional `principal` field of the
//! request wire header (or [`crate::RequestEnvelope::principal`]).  Dispatch
//! inserts it as a [`Principal`] request extension.  The field is written by
//! the host only: it is never derived from client-supplied HTTP headers, so,
//! unlike an `X-User` header a client could forge, a handler can trust it
//! exactly as far as it trusts the host.
//!
//! Extract it with [`Principal`] itself (rejects with `401` when absent),
//! `Option<Principal>` (never rejects), or plain
//! `axum::Extension<Principal>`.

use std::collections::BTreeMap;

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use http::request::Parts;
use serde::{Deserialize, Serialize};

/// The authenticated principal a **trusted host** attached to a request (the
/// `principal` wire-header field or [`crate::RequestEnvelope::principal`]).
/// Only the host writes it, never derived from client headers, so a handler
/// can trust it as far as it trusts the host.
///
/// As an extractor it **fails closed**: a handler taking `Principal`
/// answers `401 Unauthorized` ([`MissingPrincipal`]) when the host attached
/// none, instead of the `500` an absent `Extension<Principal>` produces.
/// Use `Option<Principal>` for routes that serve anonymous callers too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// Principal name (`java.security.Principal#getName`, a user id, …).
    pub name: String,
    /// Granted authorities / roles, in host order (e.g. `ROLE_ADMIN`).
    #[serde(default)]
    pub authorities: Vec<String>,
    /// Extra host-defined string attributes (tenant, token claims, …).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Principal {
    /// Whether `authority` is one of [`Self::authorities`] (exact match).
    #[must_use]
    pub fn has_authority(&self, authority: &str) -> bool {
        self.authorities.iter().any(|a| a == authority)
    }

    /// Whether the host granted the OAuth2 `scope`: an authority equal to
    /// `scope` or to `SCOPE_<scope>` — the form Spring Security's resource
    /// server (`JwtGrantedAuthoritiesConverter`) gives token scopes, which
    /// the Java bridge forwards unchanged.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.authorities
            .iter()
            .any(|a| a == scope || a.strip_prefix(SPRING_SCOPE_PREFIX) == Some(scope))
    }

    /// Require every one of `scopes` ([`Self::has_scope`]: `scope` or
    /// `SCOPE_<scope>` among [`Self::authorities`]) — the runtime counterpart
    /// of `#[route(security = [("oauth2", [...])])]`, for hosts that forward
    /// granted OAuth2 scopes as authorities.
    ///
    /// # Errors
    ///
    /// [`InsufficientScope`] listing the missing scopes (in `scopes` order);
    /// as a response it answers `403 Forbidden`.
    pub fn require_scopes(&self, scopes: &[&str]) -> Result<(), InsufficientScope> {
        let missing: Vec<String> = scopes
            .iter()
            .filter(|scope| !self.has_scope(scope))
            .map(|scope| (*scope).to_owned())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(InsufficientScope { missing })
        }
    }

    /// The attribute named `key`, if the host set one.
    #[must_use]
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }
}

/// Prefix Spring Security gives OAuth2 token scopes as authorities
/// (`SCOPE_read:users`); [`Principal::has_scope`] accepts it.
pub const SPRING_SCOPE_PREFIX: &str = "SCOPE_";

/// Rejection of the [`Principal`] extractor: the request carries no
/// principal.  Responds `401 Unauthorized` with a plain-text body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingPrincipal;

/// Body of the [`MissingPrincipal`] `401` response.
pub const MISSING_PRINCIPAL_MSG: &str = "authentication required";

impl IntoResponse for MissingPrincipal {
    fn into_response(self) -> Response {
        (StatusCode::UNAUTHORIZED, MISSING_PRINCIPAL_MSG).into_response()
    }
}

/// Rejection of [`Principal::require_scopes`]: the principal lacks required
/// scopes.  Responds `403 Forbidden` with a plain-text body and the RFC 6750
/// §3.1 challenge `WWW-Authenticate: Bearer error="insufficient_scope",
/// scope="<missing scopes>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsufficientScope {
    /// The required scopes the principal does not hold.
    pub missing: Vec<String>,
}

/// Body of the [`InsufficientScope`] `403` response.
pub const INSUFFICIENT_SCOPE_MSG: &str = "insufficient scope";

impl IntoResponse for InsufficientScope {
    fn into_response(self) -> Response {
        // RFC 7230 quoted-string: escape `\` and `"` inside the scope value.
        let scope = self
            .missing
            .join(" ")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let challenge = format!("Bearer error=\"insufficient_scope\", scope=\"{scope}\"");
        let mut response = (StatusCode::FORBIDDEN, INSUFFICIENT_SCOPE_MSG).into_response();
        if let Ok(value) = http::HeaderValue::from_str(&challenge) {
            response
                .headers_mut()
                .insert(http::header::WWW_AUTHENTICATE, value);
        }
        response
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = MissingPrincipal;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Self>()
                .cloned()
                .ok_or(MissingPrincipal),
        )
    }
}

impl<S: Send + Sync> OptionalFromRequestParts<S> for Principal {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send {
        std::future::ready(Ok(parts.extensions.get::<Self>().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::get;
    use http::Request;
    use tower::ServiceExt;

    use super::*;

    fn alice() -> Principal {
        Principal {
            name: "alice".to_owned(),
            authorities: vec!["ROLE_USER".to_owned()],
            attributes: BTreeMap::from([("tenant".to_owned(), "acme".to_owned())]),
        }
    }

    async fn call(principal: Option<Principal>, path: &str) -> (StatusCode, String) {
        let app = crate::Router::new()
            .route("/req", get(|p: Principal| async move { p.name }))
            .route(
                "/opt",
                get(|p: Option<Principal>| async move {
                    p.map_or_else(|| "anonymous".to_owned(), |p| p.name)
                }),
            );
        let mut request = Request::get(path).body(Body::empty()).unwrap();
        if let Some(principal) = principal {
            request.extensions_mut().insert(principal);
        }
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[rstest::rstest]
    #[case::required_present(Some(alice()), "/req", StatusCode::OK, "alice")]
    #[case::required_absent(None, "/req", StatusCode::UNAUTHORIZED, MISSING_PRINCIPAL_MSG)]
    #[case::optional_present(Some(alice()), "/opt", StatusCode::OK, "alice")]
    #[case::optional_absent(None, "/opt", StatusCode::OK, "anonymous")]
    #[tokio::test]
    async fn extractor_fails_closed_unless_optional(
        #[case] principal: Option<Principal>,
        #[case] path: &str,
        #[case] status: StatusCode,
        #[case] body: &str,
    ) {
        assert_eq!(call(principal, path).await, (status, body.to_owned()));
    }

    #[test]
    fn helpers_look_up_authorities_and_attributes() {
        let p = alice();
        assert!(p.has_authority("ROLE_USER"));
        assert!(!p.has_authority("ROLE_ADMIN"));
        assert_eq!(p.attribute("tenant"), Some("acme"));
        assert_eq!(p.attribute("missing"), None);
    }

    #[test]
    fn require_scopes_lists_every_missing_scope_in_order() {
        let p = Principal {
            authorities: vec!["read:users".to_owned(), "write:users".to_owned()],
            ..alice()
        };
        assert_eq!(p.require_scopes(&[]), Ok(()));
        assert_eq!(p.require_scopes(&["read:users", "write:users"]), Ok(()));
        assert_eq!(
            p.require_scopes(&["admin", "read:users", "delete:users"]),
            Err(InsufficientScope {
                missing: vec!["admin".to_owned(), "delete:users".to_owned()],
            })
        );
    }

    /// Spring Security names token scopes `SCOPE_<scope>`; the Java bridge
    /// forwards them as is, so they satisfy the plain scope.
    #[test]
    fn spring_scope_authorities_satisfy_the_scope() {
        let p = Principal {
            authorities: vec!["SCOPE_read:users".to_owned(), "ROLE_USER".to_owned()],
            ..alice()
        };
        assert!(p.has_scope("read:users"));
        assert!(!p.has_scope("SCOPE_"));
        assert!(!p.has_scope("USER"));
        assert_eq!(p.require_scopes(&["read:users"]), Ok(()));
        assert_eq!(
            p.require_scopes(&["write:users"]),
            Err(InsufficientScope {
                missing: vec!["write:users".to_owned()],
            })
        );
    }

    #[rstest::rstest]
    #[case::plain(
        &["admin", "delete:users"],
        Some(r#"Bearer error="insufficient_scope", scope="admin delete:users""#)
    )]
    #[case::quoted(&[r#"a"b\c"#], Some(r#"Bearer error="insufficient_scope", scope="a\"b\\c""#))]
    #[case::not_a_header_value(&["line\nbreak"], None)]
    #[tokio::test]
    async fn insufficient_scope_answers_403_with_a_bearer_challenge(
        #[case] missing: &[&str],
        #[case] challenge: Option<&str>,
    ) {
        let response = InsufficientScope {
            missing: missing.iter().map(|scope| (*scope).to_owned()).collect(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response
                .headers()
                .get(http::header::WWW_AUTHENTICATE)
                .map(|value| value.to_str().unwrap()),
            challenge
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, INSUFFICIENT_SCOPE_MSG.as_bytes());
    }
}
