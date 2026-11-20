//! `Validated<T>` extractor — wraps any axum `FromRequest` extractor and
//! runs the inner type's [`garde::Validate`] impl before handing the
//! value to the handler.  See [`Validated`] for usage.

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
/// `T` is `axum::Json<U>` / `axum::Form<U>` / `axum::extract::Query<U>` /
/// `axum::extract::Path<U>` where `U: serde::Deserialize +
/// garde::Validate<Context = ()>`.
///
/// # Declaring constraints with `#[schema(...)]` (recommended)
///
/// `#[derive(Schema)]` turns field-level `#[schema(...)]` constraints into
/// **both** OpenAPI keywords (`minLength`, `pattern`, `format`, `minimum`, …)
/// **and** a generated `garde::Validate` impl — no `garde` dependency is
/// needed in your own `Cargo.toml`:
///
/// ```no_run
/// use serde::Deserialize;
/// use vespera::{Schema, Validated, axum::Json};
///
/// #[derive(Deserialize, Schema)]
/// pub struct CreateUser {
///     #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
///     pub username: String,
///     #[schema(format = "email")]
///     pub email: String,
///     #[schema(minimum = 18, maximum = 120)]
///     pub age: u8,
/// }
///
/// pub async fn create(Validated(Json(req)): Validated<Json<CreateUser>>) -> &'static str {
///     // `req` has already passed validation.
///     let _ = req;
///     "ok"
/// }
/// ```
///
/// # Using `garde` directly
///
/// Alternatively derive `garde::Validate` yourself (add
/// `garde = { version = "0.23", features = ["derive"] }` plus any validator
/// features such as `email`).  Do **not** combine this with runtime
/// `#[schema(...)]` constraints on the same type — both would implement
/// `garde::Validate` (`E0119`):
///
/// ```no_run
/// use garde::Validate;
/// use serde::Deserialize;
/// use vespera::{Schema, Validated, axum::Json};
///
/// #[derive(Deserialize, Schema, Validate)]
/// pub struct CreateUser {
///     #[garde(length(min = 3, max = 32))]
///     pub username: String,
///     #[garde(email)]
///     pub email: String,
/// }
///
/// pub async fn create(Validated(Json(req)): Validated<Json<CreateUser>>) -> &'static str {
///     let _ = req;
///     "ok"
/// }
/// ```
///
/// # Rejections
///
/// A validation failure — and input that is well-formed but does not
/// deserialize into `U` (wrong type, missing field, unparsable path/query
/// value) — is rejected with `422 Unprocessable Entity` and the body
///
/// ```json
/// { "errors": [ { "message": "length is lower than 3", "path": "username" } ] }
/// ```
///
/// Other inner-extractor rejections (malformed JSON syntax `400`, missing or
/// wrong `Content-Type` `415`, body-limit errors, …) pass through unchanged.
///
/// # Non-finite floats
///
/// `Query` / `Form` / `Path` (and multipart text fields) parse an `f32` /
/// `f64` with [`str::parse`], which accepts `NaN`, `inf` and `-infinity`
/// (JSON has no such literals, so a `Json` body never spells them). A JSON
/// number beyond the range of the target type is not rejected by the default
/// build: an `f32` field given `1e39` deserializes to `f32::INFINITY` (serde
/// casts the `f64`), and the handler receives it. Only a
/// float field with a `#[schema(minimum = ..)]` / `#[schema(maximum = ..)]`
/// bound rejects a non-finite value (`"not a finite number"`, `422`); an
/// unbounded float field passes `NaN` / `±inf` to the handler although the
/// document says `type: number`. Give such a field a bound, or reject the
/// value yourself with `#[garde(custom(..))]` (e.g. a function returning an
/// error unless `value.is_finite()`).
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

#[cfg(feature = "multipart")]
impl<U, C> ValidatePayloadWith<C> for crate::multipart::TypedMultipart<U>
where
    U: Validate<Context = C>,
{
    type Inner = U;
    fn payload(&self) -> &U {
        &self.0
    }
}

#[cfg(feature = "multipart")]
impl<U, C, const MAX_TOTAL_BYTES: usize, const MAX_FIELDS: usize> ValidatePayloadWith<C>
    for crate::multipart::TypedMultipartWithLimits<U, MAX_TOTAL_BYTES, MAX_FIELDS>
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
pub struct ValidatedWith<C, T>(pub T, PhantomData<fn() -> C>);

// Written by hand, not derived: a derive would also demand `C: Clone`,
// `C: Copy` and `C: Debug`, but `C` only appears in `PhantomData<fn() -> C>`
// (which is all three for every `C`). A context type that holds a pool and
// implements none of them must not stop the wrapped payload from cloning.
impl<C, T: Clone> Clone for ValidatedWith<C, T> {
    fn clone(&self) -> Self {
        Self::new(self.0.clone())
    }
}

impl<C, T: Copy> Copy for ValidatedWith<C, T> {}

impl<C, T: std::fmt::Debug> std::fmt::Debug for ValidatedWith<C, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ValidatedWith").field(&self.0).finish()
    }
}

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
    T::Rejection: 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request(req, state)
            .await
            .map_err(rejection_response)?;
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
    T::Rejection: 'static,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request_parts(parts, state)
            .await
            .map_err(rejection_response)?;
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
    T::Rejection: 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request(req, state)
            .await
            .map_err(rejection_response)?;
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
    T::Rejection: 'static,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let extracted = T::from_request_parts(parts, state)
            .await
            .map_err(rejection_response)?;
        match extracted
            .payload()
            .validate_with(state.validation_context())
        {
            Ok(()) => Ok(Self::new(extracted)),
            Err(report) => Err(build_validation_response(&report)),
        }
    }
}

/// Turn an inner-extractor rejection into the response `Validated` returns.
///
/// Input that is well-formed but does not deserialize into the target type is
/// a *validation* failure of the same kind as a `garde` rule violation, so it
/// is reported through the SAME `422` `{"errors":[{"message","path"}]}`
/// envelope (the JNI bridge hoists exactly this shape):
///
/// * `Json` — `JsonDataError` (wrong type / missing field; axum: 422 text)
/// * `Form` — `FailedToDeserializeForm` / `FailedToDeserializeFormBody`
/// * `Query` — `FailedToDeserializeQueryString`
/// * `Path` — per-parameter parse / deserialize errors
///
/// `path` is the serde field path (`user.tags[1]` style, `""` at the root).
/// Everything else — JSON syntax errors (400), a missing/unsupported
/// `Content-Type` (415), body-limit errors, server-side `Path` misuse (500),
/// custom extractors — passes through unchanged.
fn rejection_response<R>(rejection: R) -> Response
where
    R: IntoResponse + 'static,
{
    use ::axum::extract::{
        path::ErrorKind,
        rejection::{FormRejection, JsonRejection, PathRejection, QueryRejection},
    };
    use std::any::Any;

    fn deserialize_error_entry(any: &dyn Any) -> Option<EnvelopeEntry> {
        if let Some(json) = any.downcast_ref::<JsonRejection>() {
            return match json {
                JsonRejection::JsonDataError(error) => {
                    Some(serde_error_entry(error, error.body_text()))
                }
                _ => None,
            };
        }
        if let Some(form) = any.downcast_ref::<FormRejection>() {
            return match form {
                FormRejection::FailedToDeserializeForm(error) => {
                    Some(serde_error_entry(error, error.body_text()))
                }
                FormRejection::FailedToDeserializeFormBody(error) => {
                    Some(serde_error_entry(error, error.body_text()))
                }
                _ => None,
            };
        }
        if let Some(QueryRejection::FailedToDeserializeQueryString(error)) =
            any.downcast_ref::<QueryRejection>()
        {
            return Some(serde_error_entry(error, error.body_text()));
        }
        let Some(PathRejection::FailedToDeserializePathParams(error)) =
            any.downcast_ref::<PathRejection>()
        else {
            return None;
        };
        let kind = error.kind();
        let path = match kind {
            ErrorKind::ParseErrorAtKey { key, .. }
            | ErrorKind::InvalidUtf8InPathParam { key }
            | ErrorKind::DeserializeError { key, .. } => key.clone(),
            ErrorKind::ParseErrorAtIndex { index, .. } => index.to_string(),
            ErrorKind::ParseError { .. } | ErrorKind::Message(_) => String::new(),
            // Route/extractor mismatches are server bugs (500): pass through.
            _ => return None,
        };
        Some(EnvelopeEntry {
            message: bounded_text(kind.to_string()),
            path,
        })
    }

    deserialize_error_entry(&rejection).map_or_else(
        || rejection.into_response(),
        |entry| envelope_response(&[entry]),
    )
}

/// One `{"message","path"}` entry of the 422 envelope.
struct EnvelopeEntry {
    message: String,
    path: String,
}

/// Longest envelope `message` or `path` kept verbatim (in characters).  axum
/// (`Cannot parse \`<value>\``) and serde (`invalid type: string "<value>"`)
/// echo the client's value inside the message, and serde_path_to_error records
/// map keys in the path, so a huge segment would otherwise make the `422` body
/// arbitrarily large (and past the wire hoist cap).  Multipart bounds its
/// reflected values the same way (`MAX_REFLECTED_VALUE_CHARS`).
const MAX_ENVELOPE_MESSAGE_CHARS: usize = 256;

/// `text` cut to [`MAX_ENVELOPE_MESSAGE_CHARS`] characters on a `char`
/// boundary, with a marker when shortened.  Short text is returned unchanged.
fn bounded_text(text: String) -> String {
    match text.char_indices().nth(MAX_ENVELOPE_MESSAGE_CHARS) {
        None => text,
        Some((byte_idx, _)) => format!("{}... (truncated)", &text[..byte_idx]),
    }
}

impl Serialize for EnvelopeEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut error = serializer.serialize_struct("ValidationError", 2)?;
        // Same field order as the `garde` report serializer below.
        error.serialize_field("message", &self.message)?;
        error.serialize_field("path", &self.path)?;
        error.end()
    }
}

/// Recover `(path, message)` from axum's `serde_path_to_error`-backed
/// deserialization rejections; falls back to the rejection text at the root.
fn serde_error_entry(error: &dyn std::error::Error, fallback: String) -> EnvelopeEntry {
    type JsonPathError = ::serde_path_to_error::Error<::serde_json::Error>;
    type UrlencodedPathError = ::serde_path_to_error::Error<::serde::de::value::Error>;

    // serde reports `missing field `x`` at the PARENT path; point at the field.
    let entry = |path: &::serde_path_to_error::Path, message: String| {
        let rendered = path.to_string();
        let mut path = if rendered == "." || rendered == "?" {
            String::new()
        } else {
            rendered
        };
        if let Some(field) = message
            .strip_prefix("missing field `")
            .and_then(|rest| rest.split_once('`'))
            .map(|(field, _)| field)
        {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(field);
        }
        EnvelopeEntry {
            message: bounded_text(message),
            path: bounded_text(path),
        }
    };
    let mut source = error.source();
    while let Some(current) = source {
        if let Some(inner) = current.downcast_ref::<JsonPathError>() {
            return entry(inner.path(), inner.inner().to_string());
        }
        if let Some(inner) = current.downcast_ref::<UrlencodedPathError>() {
            return entry(inner.path(), inner.inner().to_string());
        }
        source = current.source();
    }
    EnvelopeEntry {
        message: bounded_text(fallback),
        path: String::new(),
    }
}

/// `422` + `application/json` `{"errors":[...]}` from pre-built entries.
fn envelope_response(entries: &[EnvelopeEntry]) -> Response {
    struct Envelope<'a>(&'a [EnvelopeEntry]);

    impl Serialize for Envelope<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut envelope = serializer.serialize_struct("ValidationEnvelope", 1)?;
            envelope.serialize_field("errors", self.0)?;
            envelope.end()
        }
    }

    let body = ::serde_json::to_vec(&Envelope(entries))
        .unwrap_or_else(|_| FALLBACK_VALIDATION_ENVELOPE.to_vec());
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body,
    )
        .into_response()
}

/// Minimal 422 envelope emitted when serializing the real report fails.
///
/// Serializing the envelope is infallible in practice (no I/O, string keys),
/// but rendering a rejection is a request-time boundary, so the failure path
/// emits a valid envelope instead of panicking. The field order MUST stay
/// `message` then `path` to match the real serializer, so even this fallback
/// honours the snapshot-locked envelope shape.
const FALLBACK_VALIDATION_ENVELOPE: &[u8] =
    br#"{"errors":[{"message":"request validation failed","path":""}]}"#;

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
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        validation_error_body(report),
    )
        .into_response()
}

/// The `422` envelope body [`build_validation_response`] sends for `report`.
///
/// Public + `#[doc(hidden)]` (re-exported as
/// `vespera::__validation::validation_error_body`, not API) so
/// `benches/validation.rs` times this very serializer, not a mirror of it.
#[doc(hidden)]
#[must_use]
pub fn validation_error_body(report: &::garde::Report) -> Vec<u8> {
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
    ::serde_json::to_vec(&ValidationEnvelope { report })
        .unwrap_or_else(|_| FALLBACK_VALIDATION_ENVELOPE.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{FALLBACK_VALIDATION_ENVELOPE, serde_error_entry};

    /// A rejection whose source chain carries no `serde_path_to_error` error
    /// (every axum extractor wraps one today, so this guards against axum
    /// changing that) reports the rejection's own text at the root path.
    #[test]
    fn serde_error_without_path_source_falls_back_to_root_entry() {
        let error = std::io::Error::other("opaque rejection");
        let entry = serde_error_entry(&error, "fallback text".to_owned());
        assert_eq!(entry.message, "fallback text");
        assert_eq!(entry.path, "");
    }

    #[test]
    fn fallback_envelope_matches_the_snapshot_locked_shape() {
        let parsed: serde_json::Value = serde_json::from_slice(FALLBACK_VALIDATION_ENVELOPE)
            .expect("the fallback envelope must be valid JSON");
        let errors = parsed["errors"]
            .as_array()
            .expect("the fallback envelope must carry an `errors` array");

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["message"], "request validation failed");
        assert_eq!(errors[0]["path"], "");

        // The real serializer emits `message` before `path`; the fallback has to
        // agree or it drifts from the byte shape locked by the 422 snapshots.
        let text = std::str::from_utf8(FALLBACK_VALIDATION_ENVELOPE).expect("UTF-8");
        assert!(
            text.find(r#""message""#) < text.find(r#""path""#),
            "fallback field order drifted: {text}"
        );
    }

    /// Short messages pass through untouched, up to exactly the bound; one
    /// character more is cut and marked, on a character boundary.
    #[test]
    fn envelope_messages_are_bounded_at_the_character_limit() {
        let at_limit = "é".repeat(super::MAX_ENVELOPE_MESSAGE_CHARS);
        assert_eq!(super::bounded_text(at_limit.clone()), at_limit);

        let over = format!("{at_limit}x");
        let bounded = super::bounded_text(over);
        assert_eq!(bounded, format!("{at_limit}... (truncated)"));
    }

    /// A reflected value far past the bound (axum echoes the whole segment)
    /// is cut by `bounded_text` (the envelope message helper), so the
    /// message stays small.
    #[test]
    fn bounded_text_cuts_an_oversized_reflected_value() {
        let value = "a".repeat(70_000);
        let bounded = super::bounded_text(format!("Cannot parse `{value}` to a `u32`"));
        assert!(
            bounded.len() < 512,
            "bounded message is {} bytes",
            bounded.len()
        );
        assert!(bounded.ends_with("... (truncated)"));
    }

    /// Wraps a path error the way axum's rejections do: `serde_error_entry`
    /// finds the `serde_path_to_error` error by walking the source chain.
    #[derive(Debug)]
    struct Wrapped(serde_path_to_error::Error<serde_json::Error>);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrapped: {}", self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// serde_path_to_error records a map key verbatim in the path, so an
    /// oversized key used to be echoed whole in the envelope `path`.
    #[test]
    fn an_oversized_map_key_is_cut_in_the_envelope_path() {
        let key = "k".repeat(70_000);
        let body = format!(r#"{{"{key}":"not-a-number"}}"#);
        let mut de = serde_json::Deserializer::from_str(&body);
        let error =
            serde_path_to_error::deserialize::<_, std::collections::BTreeMap<String, u32>>(&mut de)
                .expect_err("a string value must not deserialize as u32");
        let wrapped = Wrapped(error);
        assert!(wrapped.to_string().contains("invalid type"), "{wrapped}");
        let entry = serde_error_entry(&wrapped, String::new());
        assert!(entry.path.len() < 512, "path is {} bytes", entry.path.len());
        assert!(entry.path.ends_with("... (truncated)"));
    }
}
