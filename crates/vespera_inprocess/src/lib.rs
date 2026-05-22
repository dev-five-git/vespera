//! In-process transport: dispatch HTTP-like requests through an axum
//! [`Router`] without a TCP socket.
//!
//! This crate is **transport-agnostic** — it knows nothing about JNI,
//! C FFI, or WASM.  It provides:
//!
//! 1. [`dispatch`] / [`dispatch_typed`] / [`dispatch_owned`] — drive a Router with an envelope
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
//!
//! # Router caching semantics
//!
//! [`register_app`] invokes the supplied factory **once** at registration
//! time and stores the resulting [`Router`].  Subsequent
//! [`dispatch_from_json`] calls reuse the cached router via
//! [`Router::clone`], which is cheap because axum's router is internally
//! `Arc`-shared.  This avoids rebuilding the route tree on every FFI
//! request.
//!
//! [`dispatch_json_with`] retains the per-call factory contract for
//! tests that do not want global state.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
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
///
/// This borrows the envelope and clones its owned fields before passing
/// them to the hot path.  Callers that already own a [`RequestEnvelope`]
/// should prefer [`dispatch_owned`] to skip the clone.
pub async fn dispatch(router: Router, envelope: &RequestEnvelope) -> String {
    let result = dispatch_owned(router, envelope.clone()).await;
    serde_json::to_string(&result).expect("ResponseEnvelope serialization is infallible")
}

/// Typed dispatch — returns a [`ResponseEnvelope`] directly.
///
/// See [`dispatch`] for the clone trade-off; prefer [`dispatch_owned`]
/// when the envelope is already owned.
pub async fn dispatch_typed(router: Router, envelope: &RequestEnvelope) -> ResponseEnvelope {
    dispatch_owned(router, envelope.clone()).await
}

/// Dispatch an owned [`RequestEnvelope`] — moves the envelope into the
/// HTTP request so the body, path, and headers are never cloned.
///
/// This is the hot path used by [`dispatch_from_json`] /
/// [`dispatch_json_with`] and is exported for callers (e.g. custom FFI
/// transports) that already own a freshly parsed envelope.
pub async fn dispatch_owned(router: Router, envelope: RequestEnvelope) -> ResponseEnvelope {
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

static APP_ROUTER: OnceLock<Router> = OnceLock::new();

/// Register a global router factory.
///
/// Any FFI boundary (JNI, C, WASM) calls this once at init time,
/// then uses [`dispatch_from_json`] on each request.
///
/// The factory is invoked **once** at registration time; the resulting
/// [`Router`] is cached and cheaply cloned on every dispatch.  Callers
/// that need to rebuild the router (e.g. for dev-only hot reload) must
/// instead pass a factory directly to [`dispatch_json_with`].
///
/// # Second-call semantics
///
/// If `register_app` has already been called in this process the second
/// (and later) calls are a **no-op** — the originally registered router
/// is preserved and the new `factory` closure is **not invoked**.  This
/// is friendlier to environments that legitimately load the cdylib twice
/// (test harnesses that re-init the global, hot-reloading JVM hosts,
/// dynamic plugin systems) than the previous panic-on-double-call
/// behaviour.  Because the new factory is never invoked, it is safe for
/// the closure to perform expensive or strictly-once work — that work
/// will not be repeated.
pub fn register_app<F>(factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    // Short-circuit if already registered.  Avoids running `factory()`
    // a second time only to drop its result.
    if APP_ROUTER.get().is_some() {
        return;
    }
    let router = factory();
    // `set` may still return `Err` if another thread won the race
    // between the `get` above and here; that is also a no-op — the
    // winning registration is preserved.
    let _ = APP_ROUTER.set(router);
}

/// Dispatch a JSON request string through the registered app.
///
/// Returns a JSON response envelope string. Requires a tokio runtime
/// on the current thread (the caller provides it — e.g. JNI crate
/// uses a `LazyLock<Runtime>`).
pub fn dispatch_from_json(input: &str, runtime: &tokio::runtime::Runtime) -> String {
    let Some(router) = APP_ROUTER.get() else {
        return serialize_error("no app registered — call register_app() at init time");
    };
    match parse_request(input) {
        Ok(envelope) => {
            let response = runtime.block_on(dispatch_owned(router.clone(), envelope));
            serde_json::to_string(&response).expect("ResponseEnvelope serialization is infallible")
        }
        Err(msg) => serialize_error(&msg),
    }
}

/// Dispatch with an explicit factory — fully testable without global state.
///
/// The factory is invoked on every call.  For the cached-router path
/// used by FFI dispatch, see [`dispatch_from_json`].
pub fn dispatch_json_with(
    input: &str,
    runtime: &tokio::runtime::Runtime,
    factory: &dyn Fn() -> Router,
) -> String {
    match parse_request(input) {
        Ok(envelope) => {
            let response = runtime.block_on(dispatch_owned(factory(), envelope));
            serde_json::to_string(&response).expect("ResponseEnvelope serialization is infallible")
        }
        Err(msg) => serialize_error(&msg),
    }
}

/// Serialize an error envelope to JSON.
pub fn serialize_error(msg: &str) -> String {
    serde_json::to_string(&error_envelope(msg)).expect("error_envelope serialization is infallible")
}

// ── Internal ─────────────────────────────────────────────────────────

async fn dispatch_inner(router: Router, envelope: RequestEnvelope) -> ResponseEnvelope {
    let version = env!("CARGO_PKG_VERSION").to_owned();

    let RequestEnvelope {
        method,
        path,
        query,
        headers,
        body,
    } = envelope;

    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };

    // Parse the HTTP method explicitly.  Previously an invalid method
    // (e.g. an empty string, whitespace, a malformed token) was
    // silently coerced to `GET`, causing the router to dispatch the
    // request to whichever handler happened to live at that path's GET
    // route.  That is a correctness footgun — a malformed method
    // would return 200 from a GET handler instead of the expected
    // method-not-allowed response.  We now short-circuit with
    // `405 Method Not Allowed` before the router is consulted.
    //
    // Note: well-formed but unknown methods (e.g. `BREW`) still reach
    // the router and let axum produce the canonical 405 itself.
    let Ok(http_method) = method.parse::<Method>() else {
        return ResponseEnvelope {
            status: 405,
            headers: HashMap::new(),
            body: format!("Method Not Allowed: '{method}' is not a valid HTTP method"),
            metadata: ResponseMetadata { version },
        };
    };

    // Case-insensitive Content-Type detection (RFC 7230 §3.2 — header
    // names are case-insensitive).  Avoids double-injecting application/json
    // when callers send "Content-Type" or "CONTENT-TYPE".
    let has_content_type = headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));

    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !body.is_empty() && !has_content_type {
        builder = builder.header("content-type", "application/json");
    }

    let request = builder
        .body(Body::from(body))
        .expect("request construction should not fail with valid URI");

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    let status = response.status().as_u16();

    // Single-pass response header conversion: collapse repeated header
    // names into HeaderValue::Multi without an intermediate
    // HashMap<String, Vec<String>>.
    let mut resp_headers: HashMap<String, HeaderValue> =
        HashMap::with_capacity(response.headers().len());
    for (name, value) in response.headers() {
        let val_str = value.to_str().unwrap_or("").to_owned();
        match resp_headers.entry(name.as_str().to_owned()) {
            Entry::Vacant(e) => {
                e.insert(HeaderValue::Single(val_str));
            }
            Entry::Occupied(mut e) => {
                let slot = e.get_mut();
                let new_slot = match std::mem::replace(slot, HeaderValue::Single(String::new())) {
                    HeaderValue::Single(prev) => HeaderValue::Multi(vec![prev, val_str]),
                    HeaderValue::Multi(mut v) => {
                        v.push(val_str);
                        HeaderValue::Multi(v)
                    }
                };
                *slot = new_slot;
            }
        }
    }

    // Body decode: avoid `Bytes -> Vec<u8> -> String` indirection.
    // `from_utf8_lossy` borrows the bytes; if they are valid UTF-8 the
    // owned String is allocated once.  Invalid sequences are replaced
    // with U+FFFD instead of being silently dropped to an empty string,
    // which surfaces non-UTF-8 responses to callers.  For true binary
    // payloads, an additive `body_bytes` field on `ResponseEnvelope`
    // remains a follow-up.
    let body_str = response.into_body().collect().await.map_or_else(
        |_| String::new(),
        |collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned(),
    );

    ResponseEnvelope {
        status,
        headers: resp_headers,
        body: body_str,
        metadata: ResponseMetadata { version },
    }
}
