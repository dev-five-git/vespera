//! In-process transport: dispatch HTTP-like requests through an axum
//! [`Router`] without a TCP socket.
//!
//! This module is **transport-agnostic** — it knows nothing about JNI,
//! C FFI, or WASM.  It converts a [`RequestEnvelope`] into an
//! [`http::Request`], drives the router via
//! [`tower::ServiceExt::oneshot`], and returns a [`ResponseEnvelope`].
//!
//! # Consumers
//!
//! * [`crate::jni`] — JNI cdylib boundary (feature `jni`)
//! * Tests — call [`dispatch`] directly
//! * CLI tools — call [`dispatch`] directly
//! * Future WASM / C FFI — same interface
//!
//! # Example
//!
//! ```ignore
//! use vespera::inprocess::{RequestEnvelope, dispatch};
//!
//! let envelope = RequestEnvelope {
//!     method: "GET".into(),
//!     path: "/health".into(),
//!     ..Default::default()
//! };
//! let json = dispatch(router, &envelope).await;
//! ```

use std::collections::HashMap;

use axum::{Router, body::Body};
use http::{Method, Request};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

// ── Envelope Types (public API) ──────────────────────────────────────

/// Inbound request envelope — JSON-serialisable representation of an
/// HTTP request that can cross any FFI boundary as a string.
#[derive(Debug, Default, Deserialize)]
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
///
/// * Single-value headers serialise as `"value"`
/// * Multi-value headers (e.g. `set-cookie`) serialise as `["v1", "v2"]`
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HeaderValue {
    /// Exactly one value for this header name.
    Single(String),
    /// Two or more values (e.g. multiple `Set-Cookie` headers).
    Multi(Vec<String>),
}

/// Metadata included in every response envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseMetadata {
    pub version: String,
}

/// Outbound response envelope — the JSON returned to the caller.
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    pub status: u16,
    pub headers: HashMap<String, HeaderValue>,
    pub body: String,
    pub metadata: ResponseMetadata,
}

// ── Core Dispatch ────────────────────────────────────────────────────

/// Dispatch a [`RequestEnvelope`] through an axum [`Router`] and
/// return the serialised [`ResponseEnvelope`] JSON.
///
/// This is the **single reusable entry-point** for driving an axum
/// router without a network socket.
pub async fn dispatch(router: Router, envelope: &RequestEnvelope) -> String {
    let result = dispatch_inner(router, envelope).await;

    serde_json::to_string(&result).unwrap_or_else(|e| {
        let version = result.metadata.version;
        format!(r#"{{"status":500,"headers":{{}},"body":"serialize: {e}","metadata":{{"version":"{version}"}}}}"#)
    })
}

/// Typed dispatch — returns a [`ResponseEnvelope`] instead of a JSON
/// string.  Useful for tests and programmatic callers that want to
/// inspect the envelope without re-parsing JSON.
pub async fn dispatch_typed(router: Router, envelope: &RequestEnvelope) -> ResponseEnvelope {
    dispatch_inner(router, envelope).await
}

/// Parse a JSON string into a [`RequestEnvelope`].
///
/// # Errors
///
/// Returns a human-readable error message if the JSON is malformed.
pub fn parse_request(json: &str) -> Result<RequestEnvelope, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid request envelope: {e}"))
}

/// Build an error [`ResponseEnvelope`] with status 500.
#[must_use]
pub fn error_envelope(message: &str) -> ResponseEnvelope {
    ResponseEnvelope {
        status: 500,
        headers: HashMap::new(),
        body: message.to_owned(),
        metadata: ResponseMetadata {
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    }
}

// ── Internal ─────────────────────────────────────────────────────────

async fn dispatch_inner(router: Router, envelope: &RequestEnvelope) -> ResponseEnvelope {
    let version = env!("CARGO_PKG_VERSION").to_owned();

    // Build URI
    let uri = if envelope.query.is_empty() {
        envelope.path.clone()
    } else {
        format!("{}?{}", envelope.path, envelope.query)
    };

    let http_method = envelope.method.parse::<Method>().unwrap_or(Method::GET);

    // Build request with all headers
    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &envelope.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !envelope.body.is_empty() && !envelope.headers.contains_key("content-type") {
        builder = builder.header("content-type", "application/json");
    }

    let request = match builder.body(Body::from(envelope.body.clone())) {
        Ok(r) => r,
        Err(e) => {
            return ResponseEnvelope {
                status: 500,
                headers: HashMap::new(),
                body: format!("failed to build request: {e}"),
                metadata: ResponseMetadata { version },
            };
        }
    };

    // Dispatch
    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    let status = response.status().as_u16();

    // Collect response headers (multi-value aware)
    let mut raw_headers: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in response.headers() {
        raw_headers
            .entry(name.as_str().to_owned())
            .or_default()
            .push(value.to_str().unwrap_or("").to_owned());
    }

    let headers = raw_headers
        .into_iter()
        .map(|(k, mut v)| {
            if v.len() == 1 {
                (k, HeaderValue::Single(v.remove(0)))
            } else {
                (k, HeaderValue::Multi(v))
            }
        })
        .collect();

    // Collect body
    let body_str = response.into_body().collect().await.map_or_else(
        |_| String::new(),
        |c| String::from_utf8(c.to_bytes().to_vec()).unwrap_or_default(),
    );

    ResponseEnvelope {
        status,
        headers,
        body: body_str,
        metadata: ResponseMetadata { version },
    }
}
