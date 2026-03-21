//! In-process transport: dispatch HTTP-like requests through an axum
//! [`Router`] without a TCP socket.
//!
//! This crate is **transport-agnostic** — it knows nothing about JNI,
//! C FFI, or WASM.  It provides:
//!
//! 1. [`dispatch`] / [`dispatch_typed`] — drive a Router with an envelope
//! 2. [`register_app`] / [`dispatch_from_json`] — global app factory
//!    for any FFI boundary (JNI, C, WASM)
//!
//! # Example (direct)
//!
//! ```ignore
//! let json = dispatch(router, &envelope).await;
//! ```
//!
//! # Example (FFI pattern)
//!
//! ```ignore
//! // At init time (e.g. JNI_OnLoad, DllMain, _start)
//! vespera_inprocess::register_app(|| create_app());
//!
//! // On each FFI call
//! let response_json = vespera_inprocess::dispatch_from_json(request_json);
//! ```

use std::collections::HashMap;
use std::sync::OnceLock;

use axum::body::Body;
use http::{Method, Request};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

/// Re-export `axum::Router` so consumers don't need a direct axum dependency.
pub use axum::Router;

// ── Envelope Types ───────────────────────────────────────────────────

/// Inbound request envelope.
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
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HeaderValue {
    Single(String),
    Multi(Vec<String>),
}

/// Metadata included in every response envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseMetadata {
    pub version: String,
}

/// Outbound response envelope.
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    pub status: u16,
    pub headers: HashMap<String, HeaderValue>,
    pub body: String,
    pub metadata: ResponseMetadata,
}

// ── Dispatch (direct) ────────────────────────────────────────────────

/// Dispatch a [`RequestEnvelope`] through an axum [`Router`] and
/// return the serialised [`ResponseEnvelope`] JSON.
pub async fn dispatch(router: Router, envelope: &RequestEnvelope) -> String {
    let result = dispatch_inner(router, envelope).await;
    serde_json::to_string(&result).expect("ResponseEnvelope serialization is infallible")
}

/// Typed dispatch — returns a [`ResponseEnvelope`] directly.
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

// ── App Factory (shared FFI pattern) ─────────────────────────────────

type AppFactory = Box<dyn Fn() -> Router + Send + Sync>;

static APP_FACTORY: OnceLock<AppFactory> = OnceLock::new();

/// Register a global router factory.
///
/// Any FFI boundary (JNI, C, WASM) calls this once at init time,
/// then uses [`dispatch_from_json`] on each request.
///
/// # Panics
///
/// Panics if called more than once.
pub fn register_app<F>(factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    assert!(
        APP_FACTORY.set(Box::new(factory)).is_ok(),
        "vespera_inprocess::register_app called more than once"
    );
}

/// Dispatch a JSON request string through the registered app.
///
/// Returns a JSON response envelope string. Requires a tokio runtime
/// on the current thread (the caller provides it — e.g. JNI crate
/// uses a `LazyLock<Runtime>`).
pub fn dispatch_from_json(input: &str, runtime: &tokio::runtime::Runtime) -> String {
    APP_FACTORY.get().map_or_else(
        || serialize_error("no app registered — call register_app() at init time"),
        |factory| dispatch_json_with(input, runtime, factory.as_ref()),
    )
}

/// Dispatch with an explicit factory — fully testable without global state.
pub fn dispatch_json_with(
    input: &str,
    runtime: &tokio::runtime::Runtime,
    factory: &dyn Fn() -> Router,
) -> String {
    match parse_request(input) {
        Ok(envelope) => runtime.block_on(dispatch(factory(), &envelope)),
        Err(msg) => serialize_error(&msg),
    }
}

/// Serialize an error envelope to JSON.
pub fn serialize_error(msg: &str) -> String {
    serde_json::to_string(&error_envelope(msg)).expect("error_envelope serialization is infallible")
}

// ── Internal ─────────────────────────────────────────────────────────

async fn dispatch_inner(router: Router, envelope: &RequestEnvelope) -> ResponseEnvelope {
    let version = env!("CARGO_PKG_VERSION").to_owned();

    let uri = if envelope.query.is_empty() {
        envelope.path.clone()
    } else {
        format!("{}?{}", envelope.path, envelope.query)
    };

    let http_method = envelope.method.parse::<Method>().unwrap_or(Method::GET);

    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &envelope.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !envelope.body.is_empty() && !envelope.headers.contains_key("content-type") {
        builder = builder.header("content-type", "application/json");
    }

    let request = builder
        .body(Body::from(envelope.body.clone()))
        .expect("request construction should not fail with valid URI");

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    let status = response.status().as_u16();

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
