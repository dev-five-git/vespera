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

use std::fmt;

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
        wanted: String,
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

impl std::error::Error for TypedMultipartError {}

impl IntoResponse for TypedMultipartError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidRequest { .. } | Self::InvalidRequestBody { .. } => {
                StatusCode::BAD_REQUEST
            }
            Self::MissingField { .. }
            | Self::DuplicateField { .. }
            | Self::UnknownField { .. }
            | Self::InvalidEnumValue { .. }
            | Self::NamelessField => StatusCode::BAD_REQUEST,
            Self::WrongFieldType { .. } => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::FieldTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Other { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
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
            .map_err(|source| TypedMultipartError::InvalidRequest { source })?;
        let value = T::try_from_multipart_with_state(&mut multipart, state).await?;
        Ok(Self(value))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Built-in TryFromFieldWithState implementations
// ═══════════════════════════════════════════════════════════════════════════════

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Read all bytes from a multipart field, enforcing an optional size limit.
///
/// When a limit is set, bytes are read incrementally via `chunk()` and the
/// cumulative size is checked after each chunk. Without a limit, `bytes()` is
/// called for a single-allocation read.
async fn read_field_data(
    mut field: Field<'_>,
    limit: Option<usize>,
) -> Result<(String, Vec<u8>), TypedMultipartError> {
    let field_name = field.name().unwrap_or_default().to_string();

    let data = if let Some(limit) = limit {
        let mut buf = Vec::new();
        while let Some(chunk) = field.chunk().await? {
            buf.extend_from_slice(&chunk);
            if buf.len() > limit {
                return Err(TypedMultipartError::FieldTooLarge {
                    field_name,
                    limit_bytes: limit,
                });
            }
        }
        buf
    } else {
        field.bytes().await?.to_vec()
    };

    Ok((field_name, data))
}

/// Parse a string as a boolean using clap-style conventions.
///
/// Accepted truthy values: `true`, `yes`, `y`, `1`, `on`
/// Accepted falsy  values: `false`, `no`, `n`, `0`, `off`
fn str_to_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" => Some(true),
        "false" | "no" | "n" | "0" | "off" => Some(false),
        _ => None,
    }
}

// ─── String ─────────────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for String {
    async fn try_from_field_with_state(
        field: Field<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let (field_name, data) = read_field_data(field, limit_bytes).await?;
        Self::from_utf8(data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name,
            wanted: "String".to_string(),
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
        let (field_name, data) = read_field_data(field, limit_bytes).await?;
        let text = std::str::from_utf8(&data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name: field_name.clone(),
            wanted: "bool".to_string(),
            source: e.to_string(),
        })?;
        str_to_bool(text).ok_or_else(|| TypedMultipartError::WrongFieldType {
            field_name,
            wanted: "bool".to_string(),
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
                    let (field_name, data) = read_field_data(field, limit_bytes).await?;
                    let text = std::str::from_utf8(&data).map_err(|e| {
                        TypedMultipartError::WrongFieldType {
                            field_name: field_name.clone(),
                            wanted: stringify!($ty).to_string(),
                            source: e.to_string(),
                        }
                    })?;
                    text.trim().parse::<$ty>().map_err(|e| {
                        TypedMultipartError::WrongFieldType {
                            field_name,
                            wanted: stringify!($ty).to_string(),
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
        let (field_name, data) = read_field_data(field, limit_bytes).await?;
        let text = std::str::from_utf8(&data).map_err(|e| TypedMultipartError::WrongFieldType {
            field_name: field_name.clone(),
            wanted: "char".to_string(),
            source: e.to_string(),
        })?;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(TypedMultipartError::WrongFieldType {
                field_name,
                wanted: "char".to_string(),
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
        let field_name = field.name().unwrap_or_default().to_string();
        let mut temp = Self::new().map_err(|e| TypedMultipartError::Other {
            source: e.to_string(),
        })?;

        let mut total = 0usize;
        while let Some(chunk) = field.chunk().await? {
            total += chunk.len();
            if let Some(limit) = limit_bytes
                && total > limit
            {
                return Err(TypedMultipartError::FieldTooLarge {
                    field_name,
                    limit_bytes: limit,
                });
            }
            std::io::Write::write_all(&mut temp, &chunk).map_err(|e| {
                TypedMultipartError::Other {
                    source: e.to_string(),
                }
            })?;
        }

        Ok(temp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_str_to_bool_truthy() {
        for val in &[
            "true", "True", "TRUE", "yes", "Yes", "y", "Y", "1", "on", "ON",
        ] {
            assert_eq!(str_to_bool(val), Some(true), "expected true for `{val}`");
        }
    }

    #[test]
    fn test_str_to_bool_falsy() {
        for val in &[
            "false", "False", "FALSE", "no", "No", "n", "N", "0", "off", "OFF",
        ] {
            assert_eq!(str_to_bool(val), Some(false), "expected false for `{val}`");
        }
    }

    #[test]
    fn test_str_to_bool_invalid() {
        for val in &["maybe", "2", "", "yep", "nah"] {
            assert_eq!(str_to_bool(val), None, "expected None for `{val}`");
        }
    }

    #[test]
    fn test_error_display() {
        let err = TypedMultipartError::MissingField {
            field_name: "name".to_string(),
        };
        assert_eq!(err.to_string(), "Missing field: `name`");

        let err = TypedMultipartError::FieldTooLarge {
            field_name: "file".to_string(),
            limit_bytes: 1024,
        };
        assert_eq!(
            err.to_string(),
            "Field `file` exceeds size limit of 1024 bytes"
        );

        let err = TypedMultipartError::WrongFieldType {
            field_name: "age".to_string(),
            wanted: "i32".to_string(),
            source: "invalid digit".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Wrong type for field `age` (expected i32): invalid digit"
        );
    }
}
