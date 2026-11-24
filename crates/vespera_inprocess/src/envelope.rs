//! Public request/response envelope types for the direct (text) API.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

// ── Envelope Types ───────────────────────────────────────────────────

/// Inbound request envelope (direct-API path).
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RequestEnvelope {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: String,
    /// Header values use the same byte mapping as the binary wire API
    /// (`header_latin1.rs`), not a UTF-8 pass-through: a value whose chars
    /// are all `<= U+00FF` maps one `char` to one byte, so `café` sent as
    /// UTF-8 text arrives as the bytes `63 61 66 E9`, not `63 61 66 C3 A9`.
    /// A value with a `char` above `U+00FF` keeps its UTF-8 bytes. Send
    /// ASCII header values when the exact bytes matter.
    ///
    /// Single value per name: a map cannot carry a repeated header, so when a
    /// name repeats the last value wins (serde keeps the last entry). The
    /// binary wire API keeps every repeat (`header_read.rs`). Changing this
    /// field's type would be a semver break, so the limit is documented here
    /// instead.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: String,
    /// Authenticated principal from the trusted embedding host (never from
    /// client headers); inserted as a [`crate::Principal`] request extension.
    #[serde(default)]
    pub principal: Option<crate::Principal>,
}

/// Response header value — single string or multiple values.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HeaderValue {
    Single(String),
    Multi(Vec<String>),
}

/// Metadata included in every response envelope.
///
/// `version` is a [`Cow`] so the engine can attach its own version
/// (`CARGO_PKG_VERSION`, a `&'static str`) without a per-response heap
/// allocation, while callers constructing envelopes manually can still
/// supply owned strings.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseMetadata {
    pub version: Cow<'static, str>,
}

impl ResponseMetadata {
    /// Metadata carrying this crate's compile-time version — zero
    /// allocation (borrows the `'static` version string).
    #[must_use]
    pub const fn current() -> Self {
        Self {
            version: Cow::Borrowed(env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Outbound response envelope.
///
/// `body` carries the response body decoded as UTF-8 text.  For
/// binary responses that are not valid UTF-8, `body` will be the
/// empty string — callers that need raw bytes must use the binary
/// wire path ([`crate::dispatch_from_bytes`]) instead of
/// [`crate::dispatch_typed`] / [`crate::dispatch_owned`].
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    pub status: u16,
    /// Header values use the wire Latin-1 mapping (`header_latin1.rs`): ASCII
    /// is unchanged, and every other value byte becomes one `char` with the
    /// same code point (`U+0000..=U+00FF`). A UTF-8 value therefore shows as
    /// mojibake: `résumé.pdf` arrives as `rÃ©sumÃ©.pdf`. Use the binary wire
    /// API when the exact header bytes matter.
    pub headers: BTreeMap<String, HeaderValue>,
    /// UTF-8 text body. Empty when the upstream response body is not
    /// valid UTF-8 (binary responses).  Use the binary wire path for
    /// faithful byte round-trips.
    pub body: String,
    pub metadata: ResponseMetadata,
}

/// Build an error [`ResponseEnvelope`] with status 500.
#[must_use]
pub fn error_envelope(message: &str) -> ResponseEnvelope {
    ResponseEnvelope {
        status: 500,
        headers: BTreeMap::new(),
        body: message.to_owned(),
        metadata: ResponseMetadata::current(),
    }
}
