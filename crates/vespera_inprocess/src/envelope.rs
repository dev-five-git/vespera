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
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: String,
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
/// wire path ([`dispatch_from_bytes`]) instead of [`dispatch_typed`]
/// / [`dispatch_owned`].
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    pub status: u16,
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
