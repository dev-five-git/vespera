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

    /// The attribute named `key`, if the host set one.
    #[must_use]
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }
}

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
}
