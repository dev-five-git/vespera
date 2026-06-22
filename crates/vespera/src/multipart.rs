//! Native multipart form data extraction for Vespera.
//!
//! Replaces the `axum_typed_multipart` crate with a zero-dependency (beyond axum)
//! implementation of typed multipart extraction. All types here are referenced by
//! the `#[derive(Multipart)]` macro's generated code.
//!
//! # Key types
//!
//! - [`TypedMultipart<T>`] — Axum extractor that parses `multipart/form-data` into `T`
//! - [`TypedMultipartError`] — Error type for multipart parsing failures
//! - [`FieldData<T>`] — Wrapper providing file metadata alongside field contents
//! - [`FieldMetadata`] — Metadata extracted from a multipart field
//! - [`TryFromMultipartWithState<S>`] — Trait for parsing a full multipart request
//! - [`TryFromFieldWithState<S>`] — Trait for parsing a single multipart field

use std::{
    borrow::Cow,
    cell::RefCell,
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use axum::extract::multipart::{Field, MultipartError, MultipartRejection};
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

// ═══════════════════════════════════════════════════════════════════════════════
// Error type
// ═══════════════════════════════════════════════════════════════════════════════

/// Errors that can occur when parsing multipart form data.
#[derive(Debug)]
pub enum TypedMultipartError {
    /// The request could not be parsed as multipart (e.g., missing Content-Type).
    InvalidRequest {
        /// The underlying rejection from axum's Multipart extractor.
        source: MultipartRejection,
    },
    /// An error occurred while reading the multipart body stream.
    InvalidRequestBody {
        /// The underlying multipart stream error.
        source: MultipartError,
    },
    /// A required field was not present in the multipart form.
    MissingField {
        /// Name of the missing field.
        field_name: String,
    },
    /// A field's value could not be parsed as the expected type.
    WrongFieldType {
        /// Name of the field.
        field_name: String,
        /// The expected type name.
        wanted: Cow<'static, str>,
        /// Description of the parse error.
        source: String,
    },
    /// A non-repeatable field appeared more than once (strict mode).
    DuplicateField {
        /// Name of the duplicate field.
        field_name: String,
    },
    /// An unrecognized field was found (strict mode only).
    UnknownField {
        /// Name of the unknown field.
        field_name: String,
    },
    /// A field's value is not a valid variant of the expected enum.
    InvalidEnumValue {
        /// Name of the field.
        field_name: String,
        /// The invalid value that was received.
        value: String,
    },
    /// A field without a name was encountered (strict mode only).
    NamelessField,
    /// A field exceeded its configured size limit.
    FieldTooLarge {
        /// Name of the field.
        field_name: String,
        /// The configured limit in bytes.
        limit_bytes: usize,
    },
    /// The cumulative bytes read across all fields exceeded the request cap.
    RequestTooLarge {
        /// Name of the field whose chunk crossed the aggregate cap.
        field_name: String,
        /// The configured aggregate limit in bytes.
        limit_bytes: usize,
    },
    /// The multipart request contained more parts than the configured cap.
    TooManyFields {
        /// The configured maximum number of fields.
        limit_fields: usize,
    },
    /// A catch-all for other errors during multipart processing.
    Other {
        /// Description of the error.
        source: String,
    },
}

/// Maximum characters of a reflected, attacker-controlled value (an invalid
/// enum variant parsed from a multipart text field) echoed back in an error.
///
/// The error `Display` feeds the serialized 4xx envelope via `collect_str`
/// ([`MultipartMessage`]), so bounding it here bounds BOTH `to_string()` and
/// the wire body — preventing a hostile field from amplifying the error
/// envelope (and its serialization cost) with a huge value.
const MAX_REFLECTED_VALUE_CHARS: usize = 128;

/// Truncate a reflected value to [`MAX_REFLECTED_VALUE_CHARS`] on a `char`
/// boundary (never mid-UTF-8), appending a marker when shortened. Borrows
/// the original when it is already within the limit (the common case).
fn truncate_reflected_value(value: &str) -> std::borrow::Cow<'_, str> {
    match value.char_indices().nth(MAX_REFLECTED_VALUE_CHARS) {
        None => std::borrow::Cow::Borrowed(value),
        Some((byte_idx, _)) => {
            std::borrow::Cow::Owned(format!("{}... (truncated)", &value[..byte_idx]))
        }
    }
}

impl fmt::Display for TypedMultipartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { source } => {
                write!(f, "Invalid multipart request: {source}")
            }
            Self::InvalidRequestBody { source } => {
                write!(f, "Invalid multipart body: {source}")
            }
            Self::MissingField { field_name } => {
                write!(f, "Missing field: `{field_name}`")
            }
            Self::WrongFieldType {
                field_name,
                wanted,
                source,
            } => {
                write!(
                    f,
                    "Wrong type for field `{field_name}` (expected {wanted}): {source}"
                )
            }
            Self::DuplicateField { field_name } => {
                write!(f, "Duplicate field: `{field_name}`")
            }
            Self::UnknownField { field_name } => {
                write!(f, "Unknown field: `{field_name}`")
            }
            Self::InvalidEnumValue { field_name, value } => {
                write!(
                    f,
                    "Invalid enum value `{}` for field `{field_name}`",
                    truncate_reflected_value(value)
                )
            }
            Self::NamelessField => write!(f, "Encountered a field without a name"),
            Self::FieldTooLarge {
                field_name,
                limit_bytes,
            } => {
                write!(
                    f,
                    "Field `{field_name}` exceeds size limit of {limit_bytes} bytes"
                )
            }
            Self::RequestTooLarge {
                field_name,
                limit_bytes,
            } => write!(
                f,
                "Multipart request exceeds aggregate size limit of {limit_bytes} bytes while reading field `{field_name}`"
            ),
            Self::TooManyFields { limit_fields } => {
                write!(
                    f,
                    "Multipart request exceeds field count limit of {limit_fields}"
                )
            }
            Self::Other { source } => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for TypedMultipartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRequest { source } => Some(source),
            Self::InvalidRequestBody { source } => Some(source),
            Self::MissingField { .. }
            | Self::WrongFieldType { .. }
            | Self::DuplicateField { .. }
            | Self::UnknownField { .. }
            | Self::InvalidEnumValue { .. }
            | Self::NamelessField
            | Self::FieldTooLarge { .. }
            | Self::RequestTooLarge { .. }
            | Self::TooManyFields { .. }
            | Self::Other { .. } => None,
        }
    }
}

impl TypedMultipartError {
    /// The offending field name when the error carries one — used as the
    /// `path` in the JSON error envelope.
    fn field_name(&self) -> Option<&str> {
        match self {
            Self::MissingField { field_name }
            | Self::WrongFieldType { field_name, .. }
            | Self::DuplicateField { field_name }
            | Self::UnknownField { field_name }
            | Self::InvalidEnumValue { field_name, .. }
            | Self::FieldTooLarge { field_name, .. }
            | Self::RequestTooLarge { field_name, .. } => Some(field_name),
            Self::InvalidRequest { .. }
            | Self::InvalidRequestBody { .. }
            | Self::NamelessField
            | Self::TooManyFields { .. }
            | Self::Other { .. } => None,
        }
    }

    /// Serialize the canonical `4xx`/`422` JSON error envelope
    /// (`{"errors":[{"message":...,"path":...}]}`) for this error — byte-
    /// identical to `Validated<T>`'s envelope so JNI hoisting and clients
    /// treat both uniformly.
    ///
    /// The message streams through [`MultipartMessage`]: `Other` (the only
    /// `500`, whose source can leak temp-file paths / OS text) yields a stable
    /// generic string; every other (client-caused) variant streams its own
    /// `Display` with NO intermediate `String`. `path` is the offending field
    /// name when known, else empty. Infallible in practice; the fallback keeps
    /// this request-time path panic-free instead of unwinding in a handler.
    fn error_body(&self) -> Vec<u8> {
        serde_json::to_vec(&MultipartErrorEnvelope {
            errors: [MultipartOneError {
                message: MultipartMessage(self),
                path: self.field_name().unwrap_or(""),
            }],
        })
        .unwrap_or_else(|_| br#"{"errors":[{"message":"serialization error","path":""}]}"#.to_vec())
    }
}

/// Stable, source-free public message for the only `500` variant (`Other`),
/// whose wrapped `source` can leak temp-file paths / OS error text. Every
/// other variant is client-caused and safe to expose verbatim.
const MULTIPART_INTERNAL_ERROR_MSG: &str = "internal error while processing multipart request";

/// Streams a multipart error's public message straight into the serializer
/// with NO intermediate `String`: `Other` becomes [`MULTIPART_INTERNAL_ERROR_MSG`];
/// every other (client-caused) variant streams its own `Display` via
/// `collect_str`. Byte-identical to the previous `to_string()`-then-serialize
/// path (serde escapes a `collect_str` stream exactly like an equal `&str`)
/// but allocation-free on the common client-error path — mirroring
/// `Validated<T>`'s 422 serializer.
struct MultipartMessage<'a>(&'a TypedMultipartError);

impl serde::Serialize for MultipartMessage<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if matches!(self.0, TypedMultipartError::Other { .. }) {
            serializer.serialize_str(MULTIPART_INTERNAL_ERROR_MSG)
        } else {
            serializer.collect_str(self.0)
        }
    }
}

/// Canonical JSON error envelope, byte-identical to `Validated<T>`'s 422
/// envelope — `{"errors":[{"message":...,"path":...}]}` (message before path)
/// — so multipart failures are consumed uniformly and, under JNI, the 422
/// body hoists into the wire header exactly like a `Validated` rejection.
/// Serialized through a borrowing `Serialize` (no `serde_json::Value`
/// map/array/object intermediate).
#[derive(serde::Serialize)]
struct MultipartOneError<'a> {
    message: MultipartMessage<'a>,
    path: &'a str,
}

#[derive(serde::Serialize)]
struct MultipartErrorEnvelope<'a> {
    errors: [MultipartOneError<'a>; 1],
}

impl IntoResponse for TypedMultipartError {
    fn into_response(self) -> Response {
        let status = match &self {
            // Preserve the SOURCE rejection / stream status so an over-limit
            // multipart body surfaces as `413 Payload Too Large` (axum's body
            // limit), an unsupported media type as `415`, etc. — instead of
            // collapsing every transport-level failure to a generic `400`.
            Self::InvalidRequest { source } => source.status(),
            Self::InvalidRequestBody { source } => source.status(),
            Self::MissingField { .. }
            | Self::DuplicateField { .. }
            | Self::UnknownField { .. }
            | Self::InvalidEnumValue { .. }
            | Self::NamelessField => StatusCode::BAD_REQUEST,
            // Scalar conversion failures are malformed field values, not an
            // unsupported multipart media type. Keep this aligned with
            // `Validated<T>`'s validation-failure status.
            Self::WrongFieldType { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::FieldTooLarge { .. }
            | Self::RequestTooLarge { .. }
            | Self::TooManyFields { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Other { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Serialize the canonical JSON error envelope (see `error_body` /
        // module-scope `MultipartErrorEnvelope`); the status varies (400/413/
        // 422/500) but the body shape is identical.
        let body = self.error_body();
        (
            status,
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response()
    }
}

impl From<MultipartError> for TypedMultipartError {
    fn from(source: MultipartError) -> Self {
        Self::InvalidRequestBody { source }
    }
}

impl From<MultipartRejection> for TypedMultipartError {
    fn from(source: MultipartRejection) -> Self {
        Self::InvalidRequest { source }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Traits
// ═══════════════════════════════════════════════════════════════════════════════

/// Parse a full multipart request body into a struct.
///
/// Typically generated by `#[derive(Multipart)]`. Each field in the struct
/// is matched against multipart field names and parsed via
/// [`TryFromFieldWithState`].
pub trait TryFromMultipartWithState<S: Send + Sync>: Sized {
    /// Parse the multipart stream into `Self`.
    fn try_from_multipart_with_state(
        multipart: &mut axum::extract::Multipart,
        state: &S,
    ) -> impl std::future::Future<Output = Result<Self, TypedMultipartError>> + Send;
}

/// Parse a single multipart field into a value.
///
/// Built-in implementations exist for `String`, `bool`, all integer and float
/// types, `char`, `tempfile::NamedTempFile`, and `FieldData<T>`.
pub trait TryFromFieldWithState<S: Send + Sync>: Sized {
    /// Parse a single field into `Self`, optionally enforcing a byte-size limit.
    fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        state: &S,
    ) -> impl std::future::Future<Output = Result<Self, TypedMultipartError>> + Send;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Field metadata
// ═══════════════════════════════════════════════════════════════════════════════

/// Metadata extracted from a multipart field part.
#[derive(Debug, Clone)]
pub struct FieldMetadata {
    /// The field name (`name` attribute in the form).
    pub name: Option<String>,
    /// The original filename (present for file uploads).
    pub file_name: Option<String>,
    /// The MIME content type of the field.
    pub content_type: Option<String>,
    /// Full HTTP headers associated with this multipart part, when explicitly captured.
    ///
    /// Vespera's built-in parsers only need `name`, `file_name`, and `content_type`,
    /// so the default `FieldData<T>` path no longer clones the whole `HeaderMap` for
    /// every field. Use [`FieldMetadata::with_headers`] when constructing metadata
    /// manually and the complete part header map is part of your API contract.
    pub headers: Option<axum::http::HeaderMap>,
}

impl FieldMetadata {
    /// Return the captured full multipart part headers, if they were collected.
    #[must_use]
    pub const fn headers(&self) -> Option<&axum::http::HeaderMap> {
        self.headers.as_ref()
    }

    /// Attach a full header snapshot to existing metadata.
    #[must_use]
    pub fn with_headers(mut self, headers: axum::http::HeaderMap) -> Self {
        self.headers = Some(headers);
        self
    }
}

impl From<&Field<'_>> for FieldMetadata {
    fn from(field: &Field<'_>) -> Self {
        Self {
            name: field.name().map(String::from),
            file_name: field.file_name().map(String::from),
            content_type: field.content_type().map(String::from),
            headers: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// FieldData<T>
// ═══════════════════════════════════════════════════════════════════════════════

/// A multipart field's parsed contents along with its metadata.
///
/// Use this wrapper when you need access to the file name, content type,
/// or other headers alongside the parsed value.
///
/// ```rust,ignore
/// use vespera::multipart::FieldData;
/// use tempfile::NamedTempFile;
///
/// #[derive(Multipart, Schema)]
/// pub struct Upload {
///     pub file: FieldData<NamedTempFile>,
/// }
/// ```
#[derive(Debug)]
pub struct FieldData<T> {
    /// Metadata about the field (name, filename, content-type, headers).
    pub metadata: FieldMetadata,
    /// The parsed contents of the field.
    pub contents: T,
}

impl<T, S> TryFromFieldWithState<S> for FieldData<T>
where
    T: TryFromFieldWithState<S> + Send,
    S: Send + Sync,
{
    async fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let metadata = FieldMetadata::from(&field);
        let contents = T::try_from_field_with_state(field, limit_bytes, state).await?;
        Ok(Self { metadata, contents })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// TypedMultipart<T> extractor
// ═══════════════════════════════════════════════════════════════════════════════

/// Axum extractor for typed multipart form data.
///
/// Wraps a struct `T` that implements [`TryFromMultipartWithState`] (typically
/// via `#[derive(Multipart)]`).
///
/// ```rust,ignore
/// use vespera::multipart::{TypedMultipart, FieldData};
/// use tempfile::NamedTempFile;
///
/// #[derive(Multipart, Schema)]
/// pub struct UploadRequest {
///     pub name: String,
///     pub file: FieldData<NamedTempFile>,
/// }
///
/// #[vespera::route(post)]
/// pub async fn upload(
///     TypedMultipart(req): TypedMultipart<UploadRequest>,
/// ) -> Json<String> {
///     Json(req.name)
/// }
/// ```
pub struct TypedMultipart<T>(pub T);

impl<T> std::ops::Deref for TypedMultipart<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for TypedMultipart<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Default aggregate cap for a typed multipart request body.
///
/// Sized as a **bounded safety budget**, not a generous allowance: it is the
/// guard that still applies when applications disable or raise axum's
/// [`DefaultBodyLimit`](axum::extract::DefaultBodyLimit) (notably the
/// in-process / JNI upload path, where axum's HTTP-layer limit never runs). At
/// 64 MiB a single request can no longer pin hundreds of MiB of buffered text
/// fields / temp-file I/O — the practical DoS budget the previous 512 MiB
/// default handed every caller. Applications that legitimately accept larger
/// typed uploads opt in explicitly via [`TypedMultipartWithLimits`] or
/// [`set_default_multipart_limits`]; genuinely large payloads should stream.
pub const DEFAULT_MULTIPART_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Default maximum number of parts in a typed multipart request.
pub const DEFAULT_MULTIPART_MAX_FIELDS: usize = 1024;

static DEFAULT_MULTIPART_TOTAL_LIMIT: AtomicUsize =
    AtomicUsize::new(DEFAULT_MULTIPART_MAX_TOTAL_BYTES);
static DEFAULT_MULTIPART_FIELD_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_MULTIPART_MAX_FIELDS);

/// Aggregate resource policy for [`TypedMultipart`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultipartLimits {
    /// Maximum cumulative bytes accepted across all parsed fields.
    pub max_total_bytes: usize,
    /// Maximum number of parsed fields accepted in one request.
    pub max_fields: usize,
}

impl MultipartLimits {
    /// Construct an aggregate multipart policy.
    #[must_use]
    pub const fn new(max_total_bytes: usize, max_fields: usize) -> Self {
        Self {
            max_total_bytes,
            max_fields,
        }
    }
}

/// Return the process-wide default aggregate multipart policy.
#[must_use]
pub fn default_multipart_limits() -> MultipartLimits {
    MultipartLimits::new(
        DEFAULT_MULTIPART_TOTAL_LIMIT.load(Ordering::Relaxed),
        DEFAULT_MULTIPART_FIELD_LIMIT.load(Ordering::Relaxed),
    )
}

/// Set the process-wide default aggregate multipart policy.
///
/// Prefer calling this during application startup, before request handling. For
/// per-route policies use [`TypedMultipartWithLimits`], which avoids global
/// process state and is therefore safer in tests and multi-tenant apps.
pub fn set_default_multipart_limits(limits: MultipartLimits) -> MultipartLimits {
    MultipartLimits::new(
        DEFAULT_MULTIPART_TOTAL_LIMIT.swap(limits.max_total_bytes, Ordering::Relaxed),
        DEFAULT_MULTIPART_FIELD_LIMIT.swap(limits.max_fields, Ordering::Relaxed),
    )
}

#[derive(Debug)]
struct MultipartAggregateState {
    limits: MultipartLimits,
    total_bytes: usize,
    fields: usize,
}

impl MultipartAggregateState {
    const fn new(limits: MultipartLimits) -> Self {
        Self {
            limits,
            total_bytes: 0,
            fields: 0,
        }
    }
}

tokio::task_local! {
    static MULTIPART_AGGREGATE: RefCell<MultipartAggregateState>;
}

/// Count one multipart PART against the request-wide `max_fields` limit.
///
/// Invoked by the derived `TryFromMultipart` loop **once per wire part** —
/// before the field name is resolved — so EVERY part (known, unknown, or
/// nameless) is counted exactly once.  Counting inside the per-known-field
/// parsers instead let unknown parts in non-strict mode (the `_ => {}`
/// dispatch arm) slip past the cap entirely, so a request with thousands of
/// unknown parts could burn unbounded parser/boundary-scan work without ever
/// tripping `TooManyFields`.
pub fn register_multipart_part() -> Result<(), TypedMultipartError> {
    MULTIPART_AGGREGATE
        .try_with(|state| {
            let mut state = state.borrow_mut();
            state.fields = state.fields.saturating_add(1);
            if state.fields > state.limits.max_fields {
                return Err(TypedMultipartError::TooManyFields {
                    limit_fields: state.limits.max_fields,
                });
            }
            Ok(())
        })
        // The derived impl can be unit-tested outside the extractor scope; with
        // no request aggregate present, counting no-ops rather than failing.
        .unwrap_or(Ok(()))
}

fn register_multipart_bytes(field_name: &str, chunk_len: usize) -> Result<(), TypedMultipartError> {
    MULTIPART_AGGREGATE
        .try_with(|state| {
            let mut state = state.borrow_mut();
            state.total_bytes = state.total_bytes.saturating_add(chunk_len);
            if state.total_bytes > state.limits.max_total_bytes {
                return Err(TypedMultipartError::RequestTooLarge {
                    field_name: field_name.to_owned(),
                    limit_bytes: state.limits.max_total_bytes,
                });
            }
            Ok(())
        })
        .unwrap_or(Ok(()))
}

/// Axum extractor variant with const aggregate multipart limits.
///
/// Use this when a route needs a tighter or looser request-level policy than
/// the process default. Per-field `#[form_data(limit = "...")]` caps still
/// apply independently: the effective policy is whichever per-field or
/// aggregate limit is exceeded first.
pub struct TypedMultipartWithLimits<
    T,
    const MAX_TOTAL_BYTES: usize,
    const MAX_FIELDS: usize = DEFAULT_MULTIPART_MAX_FIELDS,
>(pub T);

async fn parse_typed_multipart_with_limits<T, S>(
    req: Request,
    state: &S,
    limits: MultipartLimits,
) -> Result<T, TypedMultipartError>
where
    T: TryFromMultipartWithState<S>,
    S: Send + Sync + 'static,
{
    let mut multipart = axum::extract::Multipart::from_request(req, state)
        .await
        .map_err(TypedMultipartError::from)?;
    MULTIPART_AGGREGATE
        .scope(
            RefCell::new(MultipartAggregateState::new(limits)),
            async move { T::try_from_multipart_with_state(&mut multipart, state).await },
        )
        .await
}

impl<T, S> FromRequest<S> for TypedMultipart<T>
where
    T: TryFromMultipartWithState<S>,
    S: Send + Sync + 'static,
{
    type Rejection = TypedMultipartError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let value =
            parse_typed_multipart_with_limits(req, state, default_multipart_limits()).await?;
        Ok(Self(value))
    }
}

impl<T, S, const MAX_TOTAL_BYTES: usize, const MAX_FIELDS: usize> FromRequest<S>
    for TypedMultipartWithLimits<T, MAX_TOTAL_BYTES, MAX_FIELDS>
where
    T: TryFromMultipartWithState<S>,
    S: Send + Sync + 'static,
{
    type Rejection = TypedMultipartError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let value = parse_typed_multipart_with_limits(
            req,
            state,
            MultipartLimits::new(MAX_TOTAL_BYTES, MAX_FIELDS),
        )
        .await?;
        Ok(Self(value))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Built-in TryFromFieldWithState implementations
// ═══════════════════════════════════════════════════════════════════════════════

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Read all bytes from a multipart field into an owned `Vec<u8>`,
/// enforcing an optional size limit.
///
/// Bytes are accumulated chunk-by-chunk directly into the returned
/// `Vec` — the same buffer `String::from_utf8` later reuses without a
/// copy.  This deliberately avoids the previous
/// `field.bytes().await?.to_vec()` on the unlimited path, which built
/// an owned `Bytes` and then copied it into a *second* allocation,
/// doubling peak memory for large text/scalar fields.  (Returning
/// `Bytes` instead would only shift that second copy onto the `String`
/// parser, so direct `Vec` accumulation is the allocation-minimal
/// shape for every current caller.)
///
/// When a limit is set the cumulative size is checked after each chunk
/// and an over-limit chunk is rejected *before* it is copied in.
async fn read_field_data(
    mut field: Field<'_>,
    limit: Option<usize>,
    initial_capacity: usize,
) -> Result<(Field<'_>, Vec<u8>), TypedMultipartError> {
    // Part counting now happens once per part in the derived loop
    // (`register_multipart_part`), so the field parsers no longer count.
    // Initial capacity is independent from the hard byte limit: tiny scalar
    // fields keep the 256B cap without preallocating 256B per bool/number.
    let capacity = limit.map_or(initial_capacity, |limit| initial_capacity.min(limit));
    let mut buf = Vec::with_capacity(capacity);
    while let Some(chunk) = field.chunk().await? {
        register_multipart_bytes(field.name().unwrap_or_default(), chunk.len())?;
        if let Some(limit) = limit
            && buf.len().saturating_add(chunk.len()) > limit
        {
            // Reject BEFORE copying the over-limit chunk into the
            // buffer — same acceptance condition (total <= limit),
            // no wasted copy.
            return Err(TypedMultipartError::FieldTooLarge {
                field_name: field.name().unwrap_or_default().to_string(),
                limit_bytes: limit,
            });
        }
        buf.extend_from_slice(&chunk);
    }

    Ok((field, buf))
}

/// Default cap for tiny scalar multipart fields when no explicit
/// `#[form_data(limit = "...")]` is supplied. 256 bytes is far beyond any
/// legitimate bool/number/char payload while preventing unbounded buffering.
const DEFAULT_TINY_SCALAR_LIMIT_BYTES: usize = 256;
const TINY_SCALAR_INITIAL_CAPACITY_BYTES: usize = 16;
const STRING_INITIAL_CAPACITY_BYTES: usize = 64;

/// Resolve the buffering cap for a tiny scalar field: the explicit
/// per-field `#[form_data(limit = "...")]` if present, otherwise the
/// conservative [`DEFAULT_TINY_SCALAR_LIMIT_BYTES`] default.  A cap is
/// always applied — scalars never buffer unbounded input.
fn tiny_scalar_limit(limit_bytes: Option<usize>) -> usize {
    limit_bytes.unwrap_or(DEFAULT_TINY_SCALAR_LIMIT_BYTES)
}

/// Parse a string as a boolean using clap-style conventions.
///
/// Surrounding ASCII whitespace is ignored, so a multipart text value that
/// arrives with incidental padding (e.g. a trailing newline) parses like the
/// trimmed token — matching the numeric field impls, which `text.trim().parse()`.
///
/// Accepted truthy values: `true`, `yes`, `y`, `1`, `on`
/// Accepted falsy  values: `false`, `no`, `n`, `0`, `off`
fn str_to_bool(s: &str) -> Option<bool> {
    const TRUTHY: [&str; 5] = ["true", "yes", "y", "1", "on"];
    const FALSY: [&str; 5] = ["false", "no", "n", "0", "off"];
    let s = s.trim();
    if TRUTHY.iter().any(|t| s.eq_ignore_ascii_case(t)) {
        Some(true)
    } else if FALSY.iter().any(|f| s.eq_ignore_ascii_case(f)) {
        Some(false)
    } else {
        None
    }
}

// ─── String ─────────────────────────────────────────────────────────────────

/// Default buffering cap for an **unannotated** `String` multipart field.
///
/// Generous enough for any realistic text field (form text, JSON blobs,
/// small base64) yet converts the former *unbounded* accumulation into a
/// bounded one — closing a per-request memory-exhaustion vector where a
/// client could stream gigabytes into a single text field.  Opt out per
/// field with `#[form_data(limit = "unlimited")]`, or raise / lower it with
/// an explicit `#[form_data(limit = "...")]`.
const DEFAULT_STRING_FIELD_LIMIT_BYTES: usize = 1024 * 1024; // 1 MiB

/// Default streaming cap for an **unannotated** `NamedTempFile` multipart field.
///
/// The cap is intentionally larger than text fields: unannotated temp-file uploads
/// are real file uploads, but still need a denial-of-service guard by default.
/// Explicit `#[form_data(limit = "unlimited")]` continues to opt out by passing
/// `usize::MAX` through the derive-generated parser. Applications can tune the
/// process-wide default before handling requests with
/// [`set_default_temp_file_field_limit_bytes`].
///
/// Note: `"unlimited"` lifts only this **per-field** cap. The request-wide
/// aggregate budget ([`DEFAULT_MULTIPART_MAX_TOTAL_BYTES`], 64 MiB by default)
/// still applies, so a single `"unlimited"` field is bounded by the aggregate
/// rather than being truly unbounded. To raise the aggregate, use
/// [`TypedMultipartWithLimits`] (per-route) or [`set_default_multipart_limits`]
/// (process-wide); genuinely large uploads should stream instead.
pub const DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

static DEFAULT_TEMP_FILE_FIELD_LIMIT: AtomicUsize =
    AtomicUsize::new(DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES);

/// Return the current process-wide default cap for unannotated `NamedTempFile` fields.
#[must_use]
pub fn default_temp_file_field_limit_bytes() -> usize {
    DEFAULT_TEMP_FILE_FIELD_LIMIT.load(Ordering::Relaxed)
}

/// Set the process-wide default cap for unannotated `NamedTempFile` fields.
///
/// Call this during application startup, before request handling begins. Per-field
/// `#[form_data(limit = "...")]` annotations still take precedence, including the
/// explicit `"unlimited"` opt-out. The previous cap is returned to support tests or
/// embedders that need to restore their process setting.
pub fn set_default_temp_file_field_limit_bytes(limit_bytes: usize) -> usize {
    DEFAULT_TEMP_FILE_FIELD_LIMIT.swap(limit_bytes, Ordering::Relaxed)
}

impl<S: Send + Sync> TryFromFieldWithState<S> for String {
    async fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        // An ABSENT limit (`None`) applies the generous default cap; an
        // explicit `#[form_data(limit = "unlimited")]` arrives as
        // `Some(usize::MAX)` (set by the derive macro) and stays unbounded;
        // an explicit byte size wins as `Some(n)`.
        let limit = limit_bytes.unwrap_or(DEFAULT_STRING_FIELD_LIMIT_BYTES);
        let (field, data) =
            read_field_data(field, Some(limit), STRING_INITIAL_CAPACITY_BYTES).await?;
        Self::from_utf8(data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name: field.name().unwrap_or_default().to_string(),
            wanted: Cow::Borrowed("String"),
            source: e.to_string(),
        })
    }
}

// ─── bool ───────────────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for bool {
    async fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let (field, data) = read_field_data(
            field,
            Some(tiny_scalar_limit(limit_bytes)),
            TINY_SCALAR_INITIAL_CAPACITY_BYTES,
        )
        .await?;
        let text = std::str::from_utf8(&data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name: field.name().unwrap_or_default().to_string(),
            wanted: Cow::Borrowed("bool"),
            source: e.to_string(),
        })?;
        str_to_bool(text).ok_or_else(|| TypedMultipartError::WrongFieldType {
            field_name: field.name().unwrap_or_default().to_string(),
            wanted: Cow::Borrowed("bool"),
            source: format!("invalid boolean value: `{text}`"),
        })
    }
}

// ─── Numeric types ──────────────────────────────────────────────────────────

macro_rules! impl_try_from_field_for_number {
    ($($ty:ty),* $(,)?) => {
        $(
                impl<S: Send + Sync> TryFromFieldWithState<S> for $ty {
                async fn try_from_field_with_state(
                    field: Field<'_>,
                    limit_bytes: Option<usize>,
                    _state: &S,
                ) -> Result<Self, TypedMultipartError> {
                    let (field, data) = read_field_data(
                        field,
                        Some(tiny_scalar_limit(limit_bytes)),
                        TINY_SCALAR_INITIAL_CAPACITY_BYTES,
                    ).await?;
                    let text = std::str::from_utf8(&data).map_err(|e| {
                        TypedMultipartError::WrongFieldType {
                            field_name: field.name().unwrap_or_default().to_string(),
                            wanted: Cow::Borrowed(stringify!($ty)),
                            source: e.to_string(),
                        }
                    })?;
                    text.trim().parse::<$ty>().map_err(|e| {
                        TypedMultipartError::WrongFieldType {
                            field_name: field.name().unwrap_or_default().to_string(),
                            wanted: Cow::Borrowed(stringify!($ty)),
                            source: e.to_string(),
                        }
                    })
                }
            }
        )*
    };
}

impl_try_from_field_for_number!(
    i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, isize, usize, f32, f64,
);

// ─── char ───────────────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for char {
    async fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let (field, data) = read_field_data(
            field,
            Some(tiny_scalar_limit(limit_bytes)),
            TINY_SCALAR_INITIAL_CAPACITY_BYTES,
        )
        .await?;
        let text = std::str::from_utf8(&data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name: field.name().unwrap_or_default().to_string(),
            wanted: Cow::Borrowed("char"),
            source: e.to_string(),
        })?;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(TypedMultipartError::WrongFieldType {
                field_name: field.name().unwrap_or_default().to_string(),
                wanted: Cow::Borrowed("char"),
                source: "expected exactly one character".to_string(),
            }),
        }
    }
}

// ─── NamedTempFile ──────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for tempfile::NamedTempFile {
    async fn try_from_field_with_state(
        mut field: Field<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        // Part counting happens once per part in the derived loop
        // (`register_multipart_part`); the temp-file parser no longer counts.
        // Temp-file creation AND reopen() are both blocking syscalls —
        // run them together on the blocking pool so neither stalls the
        // async worker (the reopen previously ran inline on the async
        // task).  `NamedTempFile` (not `tokio::fs::File`) is retained so
        // cleanup-on-drop semantics survive; the reopened std handle is
        // wrapped in `tokio::fs` below so large writes also route to the
        // blocking pool.  `temp` keeps ownership of the path + delete-on-
        // drop guard.
        let (temp, std_file) = tokio::task::spawn_blocking(|| {
            let temp = Self::new()?;
            let std_file = temp.reopen()?;
            Ok::<_, std::io::Error>((temp, std_file))
        })
        .await
        .map_err(|e| TypedMultipartError::Other {
            source: e.to_string(),
        })?
        .map_err(|e| TypedMultipartError::Other {
            source: e.to_string(),
        })?;
        let mut file = tokio::fs::File::from_std(std_file);

        let limit_bytes = limit_bytes.unwrap_or_else(default_temp_file_field_limit_bytes);
        let mut total = 0usize;
        while let Some(chunk) = field.chunk().await? {
            register_multipart_bytes(field.name().unwrap_or_default(), chunk.len())?;
            // `saturating_add` (matching `read_field_data`) prevents a
            // pathological chunk size from wrapping `total` and slipping
            // past the limit check below.
            total = total.saturating_add(chunk.len());
            if total > limit_bytes {
                return Err(TypedMultipartError::FieldTooLarge {
                    field_name: field.name().unwrap_or_default().to_string(),
                    limit_bytes,
                });
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| TypedMultipartError::Other {
                    source: e.to_string(),
                })?;
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|e| TypedMultipartError::Other {
                source: e.to_string(),
            })?;

        Ok(temp)
    }
}

#[cfg(test)]
mod tests;
