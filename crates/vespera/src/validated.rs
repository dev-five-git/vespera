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
use ::serde::{Serialize, Serializer, ser::SerializeStruct};
use std::fmt::Display;

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

impl<U> ValidatePayload for crate::multipart::TypedMultipart<U>
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
/// { "errors": [ { "message": "...", "path": "field.name" } ] }
/// ```
///
/// Field order inside each error object is `message` then `path` —
/// matching the alphabetical order produced by the previous
/// `serde_json::json!` implementation (which used a `BTreeMap` backend).
/// The envelope shape is a public contract locked by snapshot tests and
/// the JNI wire header hoisting logic in `vespera_inprocess`.
fn build_validation_response(report: &::garde::Report) -> Response {
    struct DisplayValue<T>(T);

    impl<T> Serialize for DisplayValue<T>
    where
        T: Display,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_str(&self.0)
        }
    }

    struct ValidationEnvelope<'a> {
        report: &'a ::garde::Report,
    }

    impl Serialize for ValidationEnvelope<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut envelope = serializer.serialize_struct("ValidationEnvelope", 1)?;
            envelope.serialize_field(
                "errors",
                &ValidationErrors {
                    report: self.report,
                },
            )?;
            envelope.end()
        }
    }

    struct ValidationErrors<'a> {
        report: &'a ::garde::Report,
    }

    impl Serialize for ValidationErrors<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_seq(
                self.report
                    .iter()
                    .map(|(path, err)| ValidationError { path, err }),
            )
        }
    }

    struct ValidationError<'a> {
        path: &'a ::garde::Path,
        err: &'a ::garde::Error,
    }

    impl Serialize for ValidationError<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut error = serializer.serialize_struct("ValidationError", 2)?;
            // Keep field order byte-identical to the snapshot-locked envelope.
            error.serialize_field("message", &DisplayValue(self.err.message()))?;
            error.serialize_field("path", &DisplayValue(self.path))?;
            error.end()
        }
    }

    // Serialize straight to bytes: skips the UTF-8 re-validation that
    // `to_string` performs over `to_vec`'s output, and the body is handed
    // to axum as raw bytes (content-type is overridden to
    // application/json below regardless).  Byte-identical to the previous
    // `to_string` body.
    // Serializing the envelope is practically infallible (no I/O, string
    // keys), but this is a request-time boundary: on the unreachable failure
    // path emit a minimal valid 422 envelope rather than panicking.
    let body = ::serde_json::to_vec(&ValidationEnvelope { report }).unwrap_or_else(|_| {
        // Field order MUST match the normal serialization above (`message`
        // then `path`) so this unreachable fallback still honours the
        // snapshot-locked envelope byte shape rather than emitting a
        // path-first object that drifts from the documented contract.
        br#"{"errors":[{"message":"request validation failed","path":""}]}"#.to_vec()
    });

    let mut response = (StatusCode::UNPROCESSABLE_ENTITY, body).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        ::axum::http::HeaderValue::from_static("application/json"),
    );
    response
}
