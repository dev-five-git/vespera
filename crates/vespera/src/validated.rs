//! `Validated<T>` extractor — wraps any axum `FromRequest` extractor and
//! runs the inner type's [`garde::Validate`] impl before handing the
//! value to the handler.
//!
//! ```ignore
//! use vespera::{Validated, Schema, axum::Json};
//!
//! #[derive(serde::Deserialize, Schema)]
//! struct CreateUser {
//!     #[schema(min_length = 3, max_length = 32)]
//!     username: String,
//! }
//!
//! async fn create(Validated(Json(req)): Validated<Json<CreateUser>>)
//!     -> &'static str
//! {
//!     // `req` has already passed validation.
//!     "ok"
//! }
//! ```
//!
//! On validation failure the rejection is `422 Unprocessable Entity`
//! with a JSON body of shape:
//!
//! ```json
//! { "errors": [ { "path": "username", "message": "..." }, ... ] }
//! ```

use ::axum::{
    Json,
    extract::{FromRequest, Request},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use ::garde::Validate;

/// Extractor wrapper that validates the inner extractor's output via
/// [`garde::Validate`] before handing it to the handler.
///
/// `T` is typically `axum::Json<U>` / `axum::Form<U>` /
/// `axum::extract::Query<U>` where `U: serde::Deserialize +
/// garde::Validate<Context = ()>`.
#[derive(Debug, Clone, Copy)]
pub struct Validated<T>(pub T);

/// Helper trait that pulls the validatable payload out of common axum
/// extractors so `Validated<Json<U>>` can call `U::validate(&u, &())`.
pub trait ValidatePayload {
    /// The inner type that implements [`garde::Validate`].
    type Inner: Validate<Context = ()>;
    /// Borrow the inner value for validation.
    fn payload(&self) -> &Self::Inner;
}

impl<U> ValidatePayload for Json<U>
where
    U: Validate<Context = ()>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U> ValidatePayload for ::axum::Form<U>
where
    U: Validate<Context = ()>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U> ValidatePayload for ::axum::extract::Query<U>
where
    U: Validate<Context = ()>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U> ValidatePayload for ::axum::extract::Path<U>
where
    U: Validate<Context = ()>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<S, T> FromRequest<S> for Validated<T>
where
    S: Send + Sync,
    T: FromRequest<S> + ValidatePayload + Send,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        match extracted.payload().validate() {
            Ok(()) => Ok(Self(extracted)),
            Err(report) => Err(build_validation_response(&report)),
        }
    }
}

/// Build the canonical `422 Unprocessable Entity` response from a
/// [`garde::Report`].
///
/// Body shape:
/// ```json
/// { "errors": [ { "path": "field.name", "message": "..." } ] }
/// ```
///
/// We build the JSON via `serde_json::json!` (no extra `serde` derive
/// dep needed) so this module compiles with the bare `serde_json`
/// re-export already present on the `vespera` crate.
fn build_validation_response(report: &::garde::Report) -> Response {
    let errors: Vec<::serde_json::Value> = report
        .iter()
        .map(|(path, err)| {
            ::serde_json::json!({
                "path": path.to_string(),
                "message": err.message(),
            })
        })
        .collect();
    let envelope = ::serde_json::json!({ "errors": errors });
    let body = envelope.to_string();

    let mut response = (StatusCode::UNPROCESSABLE_ENTITY, body).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        ::axum::http::HeaderValue::from_static("application/json"),
    );
    response
}
