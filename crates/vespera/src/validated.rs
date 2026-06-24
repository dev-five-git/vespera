//! `Validated<T>` extractor — wraps any axum `FromRequest` extractor and
//! runs the inner type's [`garde::Validate`] impl before handing the
//! value to the handler.
//!
//! ```ignore
//! use vespera::{Validated, Schema, axum::Json};
//! use garde::Validate;
//!
//! #[derive(serde::Deserialize, Schema, Validate)]
//! struct CreateUser {
//!     #[schema(min_length = 3, max_length = 32)]
//!     #[garde(length(min = 3, max = 32))]
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
//! { "errors": [ { "message": "...", "path": "username" }, ... ] }
//! ```

use ::axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Request},
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE, request::Parts},
    response::{IntoResponse, Response},
};
use ::garde::Validate;
use ::serde::{Serialize, Serializer, ser::SerializeStruct};
use std::{
    fmt::Display,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

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

/// Provide the context used by [`ValidatedWith`] from axum state.
///
/// The blanket `impl<C> ValidationContext<C> for C` covers the common case
/// where `Router::with_state(ctx)` stores the validation context directly. App
/// state structs can implement this trait to expose a borrowed context field
/// without cloning per request.
pub trait ValidationContext<C> {
    /// Borrow the context used by `garde::Validate::validate_with`.
    fn validation_context(&self) -> &C;
}

impl<C> ValidationContext<C> for C {
    fn validation_context(&self) -> &C {
        self
    }
}

/// Helper trait that pulls a context-aware validatable payload out of common
/// axum extractors.
pub trait ValidatePayloadWith<C> {
    /// The inner type that implements [`garde::Validate`] with context `C`.
    type Inner: Validate<Context = C>;
    /// Borrow the inner value for validation.
    fn payload(&self) -> &Self::Inner;
}

impl<U, C> ValidatePayloadWith<C> for Json<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U, C> ValidatePayloadWith<C> for ::axum::Form<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U, C> ValidatePayloadWith<C> for ::axum::extract::Query<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U, C> ValidatePayloadWith<C> for ::axum::extract::Path<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

impl<U, C> ValidatePayloadWith<C> for crate::multipart::TypedMultipart<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

/// Context-aware validation extractor.
///
/// `Validated<T>` remains the zero-context fast path. Use
/// `ValidatedWith<C, T>` when the payload derives `garde::Validate` with
/// `#[garde(context(C))]`; the context is borrowed from axum state through
/// [`ValidationContext`].
#[derive(Debug, Clone, Copy)]
pub struct ValidatedWith<C, T>(pub T, PhantomData<fn() -> C>);

impl<C, T> ValidatedWith<C, T> {
    /// Wrap an already-extracted value. Mostly useful in tests.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self(value, PhantomData)
    }

    /// Consume the wrapper and return the extracted value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Borrow the extracted value.
    #[must_use]
    pub const fn get(&self) -> &T {
        &self.0
    }

    /// Mutably borrow the extracted value.
    #[must_use]
    pub const fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<C, T> Deref for ValidatedWith<C, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<C, T> DerefMut for ValidatedWith<C, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> ValidatePayload for T
where
    T: ValidatePayloadWith<()>,
{
    type Inner = <T as ValidatePayloadWith<()>>::Inner;

    fn payload(&self) -> &Self::Inner {
        <Self as ValidatePayloadWith<()>>::payload(self)
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

impl<S, T> FromRequestParts<S> for Validated<T>
where
    S: Send + Sync,
    T: FromRequestParts<S> + ValidatePayload + Send,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        match extracted.payload().validate() {
            Ok(()) => Ok(Self(extracted)),
            Err(report) => Err(build_validation_response(&report)),
        }
    }
}

impl<S, C, T> FromRequest<S> for ValidatedWith<C, T>
where
    S: Send + Sync + ValidationContext<C>,
    C: Send + Sync + 'static,
    T: FromRequest<S> + ValidatePayloadWith<C> + Send,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        match extracted
            .payload()
            .validate_with(state.validation_context())
        {
            Ok(()) => Ok(Self::new(extracted)),
            Err(report) => Err(build_validation_response(&report)),
        }
    }
}

impl<S, C, T> FromRequestParts<S> for ValidatedWith<C, T>
where
    S: Send + Sync + ValidationContext<C>,
    C: Send + Sync + 'static,
    T: FromRequestParts<S> + ValidatePayloadWith<C> + Send,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        match extracted
            .payload()
            .validate_with(state.validation_context())
        {
            Ok(()) => Ok(Self::new(extracted)),
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

    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body,
    )
        .into_response()
}
