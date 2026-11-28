//! Error type for typed multipart parsing.
//!
//! Extracted from the parent module so `multipart/mod.rs` stays under the
//! project-wide 1000-line cap (see AGENTS.md). The public type is re-exported
//! as `vespera::multipart::TypedMultipartError` from the parent — the public
//! API path is unchanged.

use std::borrow::Cow;
use std::fmt::{self, Write as _};

use axum::extract::multipart::{MultipartError, MultipartRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

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
///
/// `pub(super)` so the inline test module (a sibling of this file) keeps its
/// existing `super::MAX_REFLECTED_VALUE_CHARS` path — the bound is part of the
/// error type's locked-in contract, not implementation noise.
pub(super) const MAX_REFLECTED_VALUE_CHARS: usize = 128;

/// Truncate a reflected value to [`MAX_REFLECTED_VALUE_CHARS`] on a `char`
/// boundary (never mid-UTF-8), appending a marker when shortened. Borrows
/// the original when it is already within the limit (the common case).
///
/// `pub(super)` so sibling modules (`scalar_parsers`, the inline test module)
/// reach it via `super::truncate_reflected_value` exactly as they did when
/// it was defined in `multipart/mod.rs`.
pub(super) fn truncate_reflected_value(value: &str) -> std::borrow::Cow<'_, str> {
    match value.char_indices().nth(MAX_REFLECTED_VALUE_CHARS) {
        None => std::borrow::Cow::Borrowed(value),
        Some((byte_idx, _)) => {
            std::borrow::Cow::Owned(format!("{}... (truncated)", &value[..byte_idx]))
        }
    }
}

/// Maximum characters of a reflected, client-chosen part name echoed back in
/// an error message or the envelope `path` (longer names end in `…`).
pub(super) const MAX_REFLECTED_FIELD_NAME_CHARS: usize = 64;

/// Whether [`write_printable`] escapes `c`: control characters, the line and
/// paragraph separators, and the format controls (general category `Cf`)
/// that reorder, hide or join the text around them in a log line or a
/// rendered error: soft hyphen, Arabic letter / number marks, Mongolian
/// vowel separator, zero-width joiners and marks, bidi overrides and
/// isolates, word joiner and invisible operators, the byte order mark,
/// interlinear anchors, the shaping and musical controls, and the tag
/// characters. The list is hand-written (no Unicode table dependency).
fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// Write at most `max_chars` of `text`, escaping control characters
/// (`\n`, `\u{1b}`, …) so a reflected value cannot forge log lines or
/// terminal escapes, and appending `…` when it was cut.
fn write_printable(f: &mut fmt::Formatter<'_>, text: &str, max_chars: usize) -> fmt::Result {
    let mut chars = text.chars();
    for c in chars.by_ref().take(max_chars) {
        if needs_escape(c) {
            write!(f, "{}", c.escape_default())?;
        } else {
            f.write_char(c)?;
        }
    }
    if chars.next().is_some() {
        f.write_char('…')?;
    }
    Ok(())
}

/// A client-chosen part name, displayed bounded to
/// [`MAX_REFLECTED_FIELD_NAME_CHARS`] with control characters escaped.
struct Reflected<'a>(&'a str);

impl fmt::Display for Reflected<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_printable(f, self.0, MAX_REFLECTED_FIELD_NAME_CHARS)
    }
}

/// An already-bounded reflected value, displayed with control characters
/// escaped.
struct Printable<'a>(&'a str);

impl fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_printable(f, self.0, usize::MAX)
    }
}

/// The envelope `path`: the reflected part name, or `""` when the error has
/// none.  Streams through `collect_str`, which escapes exactly like a `&str`.
struct ReflectedPath<'a>(Option<&'a str>);

impl serde::Serialize for ReflectedPath<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Reflected(self.0.unwrap_or("")))
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
                write!(f, "Missing field: `{}`", Reflected(field_name))
            }
            Self::WrongFieldType {
                field_name,
                wanted,
                source,
            } => {
                write!(
                    f,
                    "Wrong type for field `{}` (expected {wanted}): {}",
                    Reflected(field_name),
                    // May quote the (bounded) client value, e.g. a bad bool.
                    Printable(source)
                )
            }
            Self::DuplicateField { field_name } => {
                write!(f, "Duplicate field: `{}`", Reflected(field_name))
            }
            Self::UnknownField { field_name } => {
                write!(f, "Unknown field: `{}`", Reflected(field_name))
            }
            Self::InvalidEnumValue { field_name, value } => {
                write!(
                    f,
                    "Invalid enum value `{}` for field `{}`",
                    Printable(&truncate_reflected_value(value)),
                    Reflected(field_name)
                )
            }
            Self::NamelessField => write!(f, "Encountered a field without a name"),
            Self::FieldTooLarge {
                field_name,
                limit_bytes,
            } => {
                write!(
                    f,
                    "Field `{}` exceeds size limit of {limit_bytes} bytes",
                    Reflected(field_name)
                )
            }
            Self::RequestTooLarge {
                field_name,
                limit_bytes,
            } => write!(
                f,
                "Multipart request exceeds aggregate size limit of {limit_bytes} bytes while reading field `{}`",
                Reflected(field_name)
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
    /// Build an invalid-enum error while bounding the attacker-controlled value stored in it.
    #[must_use]
    pub fn invalid_enum_value(field_name: String, value: &str) -> Self {
        Self::InvalidEnumValue {
            field_name,
            value: truncate_reflected_value(value).into_owned(),
        }
    }

    /// Build a wrong-type error for a client `value` that could not be parsed
    /// as `wanted`.  The `source` quotes the client value in backticks after
    /// `"invalid value: "`, bounded like [`Self::invalid_enum_value`]'s, so
    /// a caller never has to bound it by hand.  Use the `WrongFieldType`
    /// variant directly only for a message that does not quote the value.
    #[must_use]
    pub fn wrong_field_type_value(field_name: String, wanted: &'static str, value: &str) -> Self {
        Self::WrongFieldType {
            field_name,
            wanted: Cow::Borrowed(wanted),
            source: format!("invalid value: `{}`", truncate_reflected_value(value)),
        }
    }

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
    /// The message streams through [`MultipartMessage`]: `Other` (whose source
    /// can leak temp-file paths / OS text) yields a stable generic string;
    /// every other variant streams its own `Display` with NO intermediate
    /// `String`. `path` is the offending field
    /// name when known, else empty. Infallible in practice; the fallback keeps
    /// this request-time path panic-free instead of unwinding in a handler.
    ///
    /// Public + `#[doc(hidden)]` (not API) so the inline test module can
    /// snapshot the wire body and `benches/multipart_error.rs` times this very
    /// serializer, not a mirror of it — the `IntoResponse` impl below remains
    /// the only production call site.
    #[doc(hidden)]
    #[must_use]
    pub fn error_body(&self) -> Vec<u8> {
        serde_json::to_vec(&MultipartErrorEnvelope {
            errors: [MultipartOneError {
                message: MultipartMessage(self),
                path: ReflectedPath(self.field_name()),
            }],
        })
        .unwrap_or_else(|_| br#"{"errors":[{"message":"serialization error","path":""}]}"#.to_vec())
    }
}

/// Stable, source-free public message for `Other`, whose wrapped `source` can
/// leak temp-file paths / OS error text. Every other variant's `Display` is
/// safe to expose verbatim: the `InvalidRequestBody` sources are generic
/// (axum's `MultipartError` text never carries the body's contents).
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
    path: ReflectedPath<'a>,
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
            // axum maps a non-limit body read failure (`StreamReadFailed`, e.g.
            // a client that aborted the upload) to `500` too; it is reported as
            // axum's status rather than rewritten here.
            Self::InvalidRequestBody { source } => source.status(),
            Self::DuplicateField { .. } | Self::UnknownField { .. } | Self::NamelessField => {
                StatusCode::BAD_REQUEST
            }
            // A missing required field, scalar conversion failures and unknown
            // enum variants are malformed field values, not an unsupported
            // multipart media type or a malformed form. Keep this aligned with
            // `Validated<T>`'s rejection status (a missing field in a
            // `Validated<Json<_>>` body is a `422` too, and so is an unknown
            // variant), so under JNI all of them hoist into `validation_errors`
            // the same way.
            Self::MissingField { .. }
            | Self::WrongFieldType { .. }
            | Self::InvalidEnumValue { .. } => StatusCode::UNPROCESSABLE_ENTITY,
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

#[cfg(test)]
mod tests {
    use super::TypedMultipartError;

    /// A client value far past the bound is cut in the quoted `source`, so
    /// both the `Display` (`to_string`) and the 4xx envelope stay small.
    #[test]
    fn wrong_field_type_value_bounds_the_quoted_value_in_display_and_envelope() {
        let value = "a".repeat(70_000);
        let error = TypedMultipartError::wrong_field_type_value("flag".to_owned(), "bool", &value);
        let display = error.to_string();
        assert!(display.len() < 512, "display is {} bytes", display.len());
        assert!(display.contains("(truncated)"), "{display}");
        let body = error.error_body();
        assert!(body.len() < 1024, "envelope is {} bytes", body.len());
        let text = String::from_utf8(body).expect("UTF-8 envelope");
        assert!(text.contains("(truncated)"), "{text}");
        assert!(text.contains("Wrong type for field `flag`"), "{text}");
    }

    /// A short value is quoted in full.
    #[test]
    fn wrong_field_type_value_quotes_a_short_value_verbatim() {
        let error = TypedMultipartError::wrong_field_type_value("n".to_owned(), "u32", "x1");
        assert_eq!(
            error.to_string(),
            "Wrong type for field `n` (expected u32): invalid value: `x1`"
        );
    }
}
