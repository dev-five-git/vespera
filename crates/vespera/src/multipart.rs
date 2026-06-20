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

use std::{borrow::Cow, fmt};

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
    /// A catch-all for other errors during multipart processing.
    Other {
        /// Description of the error.
        source: String,
    },
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
                write!(f, "Invalid enum value `{value}` for field `{field_name}`")
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
            | Self::FieldTooLarge { field_name, .. } => Some(field_name),
            Self::InvalidRequest { .. }
            | Self::InvalidRequestBody { .. }
            | Self::NamelessField
            | Self::Other { .. } => None,
        }
    }

    /// Public-facing message for the JSON error envelope.
    ///
    /// `Other` wraps internal I/O / blocking-task failures whose source
    /// string can leak implementation details (temp-file paths, OS error
    /// text); it is the only `500` variant, so it returns a stable, generic
    /// message. Every other variant returns its `Display` (already safe —
    /// it describes a client-supplied field problem). The full `Display`
    /// (including `Other`'s `source`) stays available for server-side
    /// logging via the `std::error::Error` impl.
    fn response_message(&self) -> Cow<'_, str> {
        if matches!(self, Self::Other { .. }) {
            Cow::Borrowed("internal error while processing multipart request")
        } else {
            Cow::Owned(self.to_string())
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
    message: &'a str,
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
            Self::FieldTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Other { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Serialize the canonical 422 envelope (see module-scope
        // `MultipartErrorEnvelope` / `MultipartOneError`); `path` is the
        // offending field name when known, else empty.
        let path = self.field_name().unwrap_or("");
        let message = self.response_message();
        let body = serde_json::to_vec(&MultipartErrorEnvelope {
            errors: [MultipartOneError {
                message: &message,
                path,
            }],
        })
        // Serializing a struct of two `&str` is infallible in practice; the
        // fallback keeps this request-time error path panic-free (matching
        // `Validated<T>`'s 422 envelope) by emitting a minimal valid envelope
        // instead of unwinding inside a handler.
        .unwrap_or_else(|_| {
            br#"{"errors":[{"message":"serialization error","path":""}]}"#.to_vec()
        });
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
    /// All HTTP headers associated with this multipart part.
    pub headers: axum::http::HeaderMap,
}

impl From<&Field<'_>> for FieldMetadata {
    fn from(field: &Field<'_>) -> Self {
        Self {
            name: field.name().map(String::from),
            file_name: field.file_name().map(String::from),
            content_type: field.content_type().map(String::from),
            headers: field.headers().clone(),
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

impl<T, S> FromRequest<S> for TypedMultipart<T>
where
    T: TryFromMultipartWithState<S>,
    S: Send + Sync + 'static,
{
    type Rejection = TypedMultipartError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let mut multipart = axum::extract::Multipart::from_request(req, state)
            .await
            .map_err(TypedMultipartError::from)?;
        let value = T::try_from_multipart_with_state(&mut multipart, state).await?;
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
    // Initial capacity is independent from the hard byte limit: tiny scalar
    // fields keep the 256B cap without preallocating 256B per bool/number.
    let capacity = limit.map_or(initial_capacity, |limit| initial_capacity.min(limit));
    let mut buf = Vec::with_capacity(capacity);
    while let Some(chunk) = field.chunk().await? {
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
/// Accepted truthy values: `true`, `yes`, `y`, `1`, `on`
/// Accepted falsy  values: `false`, `no`, `n`, `0`, `off`
fn str_to_bool(s: &str) -> Option<bool> {
    const TRUTHY: [&str; 5] = ["true", "yes", "y", "1", "on"];
    const FALSY: [&str; 5] = ["false", "no", "n", "0", "off"];
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
/// Explicit `#[form_data(limit = "unlimited")]` continues to opt out by passing
/// `usize::MAX` through the derive-generated parser.
const DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES: usize = 1024 * 1024; // 1 MiB

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

        let limit_bytes = limit_bytes.unwrap_or(DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES);
        let mut total = 0usize;
        while let Some(chunk) = field.chunk().await? {
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
