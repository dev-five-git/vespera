//! In-process transport: dispatch HTTP-like requests through an axum
//! [`Router`] without a TCP socket.
//!
//! This crate is **transport-agnostic** — it knows nothing about JNI,
//! C FFI, or WASM.  It exposes two API layers on top of a single
//! shared dispatch core:
//!
//! 1. **Direct API** — [`dispatch`] / [`dispatch_typed`] /
//!    [`dispatch_owned`] drive a [`Router`] with a [`RequestEnvelope`]
//!    and return a [`ResponseEnvelope`].  Bodies on this path are
//!    UTF-8 text only; if the upstream response body is not valid
//!    UTF-8 (binary content), [`ResponseEnvelope::body`] is the
//!    empty string.  Callers that need raw bytes must use the
//!    binary wire API below.
//!
//! 2. **Binary wire API** — [`dispatch_from_bytes`] is the
//!    zero-overhead FFI entry point.  Wire format (request and
//!    response use the same layout):
//!
//!    ```text
//!    bytes 0..4      : u32 BE = header_json byte length N
//!    bytes 4..4+N    : UTF-8 JSON
//!                        (request)  { "v":1, "method", "path",
//!                                     "query"?, "headers"? }
//!                        (response) { "v":1, "status", "headers",
//!                                     "metadata" }
//!    bytes 4+N..end  : raw body bytes (UTF-8 text or binary —
//!                       no encoding applied)
//!    ```
//!
//!    All failure modes return a valid wire-format response so the
//!    caller's decoder never has to special-case errors.
//!
//! # Example (direct)
//!
//! ```ignore
//! let response = dispatch_typed(router, &envelope).await;
//! ```
//!
//! # Example (binary wire / FFI)
//!
//! ```ignore
//! // At init time (e.g. JNI_OnLoad, DllMain, _start)
//! vespera_inprocess::register_app(|| create_app());
//!
//! // On each FFI call
//! let response_bytes =
//!     vespera_inprocess::dispatch_from_bytes(request_bytes, &runtime);
//! ```
//!
//! # Router caching semantics
//!
//! [`register_app`] invokes the supplied factory **once** at
//! registration time and stores the resulting [`Router`].  Subsequent
//! [`dispatch_from_bytes`] calls reuse the cached router via
//! [`Router::clone`], which is cheap because axum's router is
//! internally `Arc`-shared.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::btree_map::Entry;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{LazyLock, RwLock};
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http::{Method, Request};
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

/// Re-export `axum::Router` so consumers don't need a direct axum dependency.
pub use axum::Router;

/// Wire format protocol version.  The JSON header's `v` field MUST
/// equal this for requests; responses always emit this value.
const WIRE_VERSION: u8 = 1;

/// Canonical name of the default app — used when the wire header
/// omits `"app"` or sets it to an empty string, and when callers use
/// the BC [`register_app`] entry point.
pub const DEFAULT_APP_NAME: &str = "_default";

/// Maximum allowed length of an app name (after trimming).  Sized so
/// names fit comfortably in URL path segments and log lines.
const MAX_APP_NAME_LEN: usize = 64;

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

// ── Wire Format Types (internal) ─────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WireRequestHeader {
    /// Wire protocol version; clients MUST send 1.
    #[serde(default)]
    v: u8,
    method: String,
    path: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    /// Optional name of the target app for multi-app routing.  When
    /// omitted (or empty), the request is dispatched to the default
    /// app registered via [`register_app`].  Use [`register_app_named`]
    /// to register additional named apps.
    #[serde(default)]
    app: Option<String>,
}

#[derive(Debug, Serialize)]
struct WireResponseHeader<'a> {
    v: u8,
    status: u16,
    headers: &'a BTreeMap<String, HeaderValue>,
    metadata: &'a ResponseMetadata,
    /// Validation errors hoisted from a 422 JSON body so Java decoders
    /// can read them with a single header parse.  `None` for any other
    /// status; the original body is preserved verbatim regardless.
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_errors: Option<Vec<ValidationErrorItem>>,
}

/// One entry in the wire header's `validation_errors` array.  Fields
/// are best-effort: missing values in the source body become `None`.
#[derive(Debug, Serialize)]
struct ValidationErrorItem {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

// ── Dispatch (direct API — backward compatible) ──────────────────────

/// Dispatch a [`RequestEnvelope`] through an axum [`Router`] and
/// return the serialised [`ResponseEnvelope`] JSON.
///
/// This borrows the envelope and clones its owned fields before
/// passing them to the hot path.  Callers that already own a
/// [`RequestEnvelope`] should prefer [`dispatch_owned`] to skip the
/// clone.
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

/// Dispatch an owned [`RequestEnvelope`] — moves the envelope into
/// the HTTP request so the body, path, and headers are never cloned.
///
/// This is the hot path used by callers (e.g. custom FFI transports)
/// that already own a freshly built envelope.
pub async fn dispatch_owned(router: Router, envelope: RequestEnvelope) -> ResponseEnvelope {
    let parts = match dispatch_parts(
        router,
        &envelope.method,
        envelope.path,
        envelope.query,
        envelope.headers,
        Bytes::from(envelope.body),
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            return ResponseEnvelope {
                status,
                headers: BTreeMap::new(),
                body: msg,
                metadata: ResponseMetadata::current(),
            };
        }
    };
    to_response_envelope_text(parts)
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

// ── App Factory (shared FFI pattern) ─────────────────────────────────

/// Per-name router cache.  Indexed by app name; the default app uses
/// [`DEFAULT_APP_NAME`] (`"_default"`).
///
/// Uses [`RwLock`] (not [`OnceLock`]) so multiple named apps can be
/// registered after init time, while keeping dispatch reads
/// contention-free.  The map is read on every dispatch and written
/// only during `register_app*` calls (typically at process startup).
///
/// Lock poisoning recovery: every read path uses
/// `unwrap_or_else(|e| e.into_inner())` so a panic in a producer
/// thread does not lock out the dispatch hot path.  Factory closures
/// are also invoked **outside** the write lock so a factory panic
/// cannot poison the map.
static APP_ROUTERS: LazyLock<RwLock<HashMap<String, Router>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Validate an app name for registration / lookup.
///
/// Constraints:
/// - non-empty after trimming whitespace
/// - at most [`MAX_APP_NAME_LEN`] bytes
/// - ASCII alphanumeric, `_`, or `-` only
///
/// Returns the trimmed name on success.
fn validate_app_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("app name must not be empty".to_owned());
    }
    if trimmed.len() > MAX_APP_NAME_LEN {
        return Err(format!(
            "app name too long: {} chars (max {MAX_APP_NAME_LEN})",
            trimmed.len()
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "app name '{trimmed}' contains invalid characters (allowed: alphanumeric, '_', '-')"
        ));
    }
    Ok(trimmed)
}

/// Register the **default** global router factory.
///
/// Equivalent to `register_app_named(DEFAULT_APP_NAME, factory)`.
/// Wire requests without an `"app"` header (or with `"app": ""`) are
/// routed here.
///
/// Any FFI boundary (JNI, C, WASM) calls this once at init time, then
/// uses [`dispatch_from_bytes`] on each request.
///
/// # Second-call semantics
///
/// Calling `register_app` more than once is a **no-op** — the first
/// registration wins, the new factory closure is NOT invoked.  Friendly
/// for environments that legitimately load the cdylib twice (hot-reloading
/// JVM hosts, plugin systems).
pub fn register_app<F>(factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    register_app_named(DEFAULT_APP_NAME, factory);
}

/// Register a **named** global router factory for multi-app routing.
///
/// Wire requests carrying `"app": "<name>"` in their header are
/// dispatched to this router.  Multiple named apps can coexist in
/// the same process; register each once at init time.
///
/// # First-wins per name
///
/// Calling this more than once with the same `name` is a no-op — the
/// first registration wins.  Registering different names is the
/// supported multi-app pattern.
///
/// # Panic safety
///
/// The `factory` closure is invoked **outside** the internal
/// `RwLock`'s write guard.  A panic in `factory` cannot poison the
/// map; the registration is simply discarded and the slot remains
/// available for retry.
///
/// # Invalid names
///
/// Names that fail [`validate_app_name`] (empty, > 64 bytes, or
/// containing characters outside `[A-Za-z0-9_-]`) are silently
/// discarded — registration is a no-op.  Dispatch with a matching
/// invalid name will return a `400` wire response.
pub fn register_app_named<F>(name: &str, factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    let name = match validate_app_name(name) {
        Ok(n) => n.to_owned(),
        Err(_) => return,
    };
    // Fast path: existence check under a read lock.
    {
        let map = APP_ROUTERS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.contains_key(&name) {
            return;
        }
    }
    // Build the router OUTSIDE the write lock so a panicking factory
    // cannot poison the map.
    let router = factory();
    let mut map = APP_ROUTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Double-check: another thread may have inserted between our read
    // and write.  First-wins still holds — use Entry to avoid the
    // map.contains_key + map.insert double lookup.
    map.entry(name).or_insert(router);
}

/// Resolve a [`Router`] for a wire request, applying default-app
/// fallback and name validation.  Returns the cloned router (cheap —
/// axum's router is `Arc`-backed) on success, or a wire error response
/// (`400` for invalid name, `404` for unregistered name) on failure.
///
/// Lookup-first: registered names are validated at registration time
/// ([`register_app_named`] discards invalid names), so a map hit is
/// valid by construction.  Validation runs only on a miss, purely to
/// pick the right error status (`400` invalid vs `404` unregistered)
/// — keeping the per-request hot path to trim + hash lookup.
fn resolve_app_router(header: &WireRequestHeader) -> Result<Router, Vec<u8>> {
    let name = header
        .app
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_APP_NAME);
    {
        let map = APP_ROUTERS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(router) = map.get(name) {
            return Ok(router.clone());
        }
    }
    // Miss: decide between 400 (invalid name) and 404 (unregistered).
    match validate_app_name(name) {
        Err(msg) => Err(error_wire(400, &format!("invalid app name: {msg}"))),
        Ok(name) => Err(error_wire(
            404,
            &format!(
                "no app registered with name '{name}' — \
                 use register_app() for the default app or \
                 register_app_named(name, factory) for additional apps"
            ),
        )),
    }
}

// ── Binary Wire API ──────────────────────────────────────────────────

/// Dispatch a wire-format request through the registered app and
/// return a wire-format response.
///
/// Wire format:
/// ```text
/// bytes 0..4      : u32 BE = header_json byte length N
/// bytes 4..4+N    : UTF-8 JSON
///                     (request)  { "v":1, "method", "path",
///                                  "query"?, "headers"? }
///                     (response) { "v":1, "status", "headers",
///                                  "metadata" }
/// bytes 4+N..end  : raw body bytes (UTF-8 text or binary —
///                   no encoding applied)
/// ```
///
/// All failure modes return a valid wire-format response (length-
/// prefixed) so the caller's decoder never has to special-case
/// errors.  Specifically:
///
/// * input shorter than 4 bytes → 400 with explanatory body
/// * `header_len` exceeds input → 400
/// * header JSON parse failure → 400
/// * wire version mismatch → 400
/// * invalid app name → 400
/// * unknown HTTP method → 405
/// * no app registered under the requested name → 404
/// * router/handler errors → surfaced verbatim as response wire
pub fn dispatch_from_bytes(input: Vec<u8>, runtime: &tokio::runtime::Runtime) -> Vec<u8> {
    runtime.block_on(dispatch_from_bytes_async(input))
}

/// **Streaming** sibling of [`dispatch_from_bytes_async`].
///
/// Drives the dispatch end-to-end like the non-streaming variant but
/// emits the response body **chunk-by-chunk via `on_chunk`** instead
/// of materialising it in a single `Vec<u8>`.  Returns the wire-format
/// header bytes only (`[u32 BE header_len | header JSON]`) — the body
/// is delivered through the callback while the dispatch is in flight,
/// so a 1 GiB response is never resident in memory.
///
/// `on_chunk` is invoked one or more times in arrival order; the
/// borrowed slice is valid only for the duration of each call and the
/// callback should treat it as ephemeral (e.g. write it to an
/// `OutputStream`, accumulate it on disk, …).
///
/// Failure modes are identical to [`dispatch_from_bytes_async`] —
/// returns a valid wire-format error response (header + body) when
/// the wire input is malformed, the version is wrong, no app is
/// registered, or the handler reports a pre-dispatch error.  In the
/// error path the body is included inside the returned bytes (not
/// streamed via `on_chunk`) because the error message is small.
///
/// `on_chunk` is NOT called if the response body is empty.
pub async fn dispatch_streaming_async<F>(input: Vec<u8>, mut on_chunk: F) -> Vec<u8>
where
    F: FnMut(&[u8]),
{
    let (header, body_bytes) = match parse_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return error_wire(400, &msg),
    };
    if header.v != WIRE_VERSION {
        return error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        );
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => return wire,
    };
    let (status, headers, metadata) = match dispatch_response_streaming(
        router,
        &header.method,
        header.path,
        header.query,
        header.headers,
        body_bytes,
        &mut on_chunk,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return error_wire(status, &msg),
    };
    // Emit header-only wire bytes; body was streamed via on_chunk.
    let header_view = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers: &headers,
        metadata: &metadata,
        // Streaming path does not hoist 422 validation errors —
        // hoisting requires materialising the full body, which is
        // antithetical to the streaming contract.  Callers needing
        // validation hoisting should use dispatch_from_bytes_async.
        validation_errors: None,
    };
    let header_json =
        serde_json::to_vec(&header_view).expect("WireResponseHeader serialization is infallible");
    let header_len =
        u32::try_from(header_json.len()).expect("response header JSON exceeds u32::MAX bytes");
    let mut out = Vec::with_capacity(4 + header_json.len());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_json);
    out
}

/// Async sibling of [`dispatch_from_bytes`].  Use this when the caller
/// is already inside a Tokio runtime (e.g. an axum handler embedding
/// another vespera router, or a tokio-spawned task in the JNI bridge's
/// async dispatch path).
///
/// All failure modes return a valid wire-format response (same
/// guarantees as [`dispatch_from_bytes`]), including `404` when no app
/// is registered under the requested name.
pub async fn dispatch_from_bytes_async(input: Vec<u8>) -> Vec<u8> {
    // Wire-level checks first: malformed input must report parse
    // errors regardless of whether an app is registered.
    let (header, body_bytes) = match parse_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return error_wire(400, &msg),
    };
    if header.v != WIRE_VERSION {
        return error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        );
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => return wire,
    };
    let parts = match dispatch_parts(
        router,
        &header.method,
        header.path,
        header.query,
        header.headers,
        body_bytes,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return error_wire(status, &msg),
    };
    to_wire_bytes(parts)
}

/// Outcome of [`dispatch_into_async`] / [`dispatch_into`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectWriteResult {
    /// A complete wire response occupies `out[0..n]`.
    Complete(usize),
    /// The response needs `required` bytes and `out` was too small.
    /// `out` contents are **undefined** (a prefix may have been
    /// written).  `required` is exact — a retry with a buffer of at
    /// least this size succeeds, but **re-runs the handler**.
    Overflow(usize),
}

/// Sync wrapper around [`dispatch_into_async`] for FFI callers that
/// own a [`tokio::runtime::Runtime`].
pub fn dispatch_into(
    input: Vec<u8>,
    out: &mut [u8],
    runtime: &tokio::runtime::Runtime,
) -> DirectWriteResult {
    runtime.block_on(dispatch_into_async(input, out))
}

/// Dispatch a wire-format request and write the wire response
/// **directly into `out`** — the zero-materialisation sibling of
/// [`dispatch_from_bytes_async`].
///
/// On the success path the response is never assembled in an
/// intermediate `Vec`: the wire header is written to `out[0..h]` as
/// soon as axum produces status + headers, then each body frame is
/// copied straight to its final offset.  Compared with
/// `dispatch_from_bytes_async` + caller-side copy, this removes one
/// full response memcpy and the response-sized allocation.
///
/// # Exceptions to direct writing
///
/// * **`422` responses** are materialised first so the
///   `validation_errors` hoisting into the wire header (see
///   [`dispatch_from_bytes`]) is preserved byte-for-byte — validation
///   failures are tiny and cold, correctness wins.
/// * **Pre-dispatch errors** (malformed wire, bad version, unknown
///   app, invalid method) write the small `error_wire` response.
///
/// # Overflow semantics
///
/// If `out` is too small the body stream is still drained (counting,
/// not writing) so [`DirectWriteResult::Overflow`] reports the
/// **exact** required size.  The handler has already run; retrying
/// runs it again — callers must gate retries on idempotency.
pub async fn dispatch_into_async(input: Vec<u8>, out: &mut [u8]) -> DirectWriteResult {
    let (header, body_bytes) = match parse_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return write_wire_into(out, &error_wire(400, &msg)),
    };
    if header.v != WIRE_VERSION {
        return write_wire_into(
            out,
            &error_wire(
                400,
                &format!(
                    "unsupported wire version: got {}, expected {WIRE_VERSION}",
                    header.v
                ),
            ),
        );
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => return write_wire_into(out, &wire),
    };

    let (status, headers, metadata, mut body) = match dispatch_and_split(
        router,
        &header.method,
        header.path,
        header.query,
        header.headers,
        Body::from(body_bytes),
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return write_wire_into(out, &error_wire(status, &msg)),
    };

    if status == 422 {
        // Materialise to preserve validation_errors hoisting in the
        // wire header — identical bytes to dispatch_from_bytes.
        let body_bytes = body
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .unwrap_or_default();
        let wire = to_wire_bytes((status, headers, body_bytes, metadata));
        return write_wire_into(out, &wire);
    }

    let header_bytes = build_wire_header_bytes(status, &headers, &metadata);
    let mut written = 0usize;
    if header_bytes.len() <= out.len() {
        out[..header_bytes.len()].copy_from_slice(&header_bytes);
        written = header_bytes.len();
    }
    let mut required = header_bytes.len();

    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            let len = data.len();
            // Write only while the output is still contiguous
            // (`written == required` ⇒ nothing has been skipped yet).
            if written == required && written + len <= out.len() {
                out[written..written + len].copy_from_slice(data);
                written += len;
            }
            required += len;
        }
    }

    if written == required {
        DirectWriteResult::Complete(written)
    } else {
        DirectWriteResult::Overflow(required)
    }
}

/// Copy a fully-assembled wire response into `out`, or report the
/// exact required size.
fn write_wire_into(out: &mut [u8], wire: &[u8]) -> DirectWriteResult {
    if wire.len() <= out.len() {
        out[..wire.len()].copy_from_slice(wire);
        DirectWriteResult::Complete(wire.len())
    } else {
        DirectWriteResult::Overflow(wire.len())
    }
}

/// Build a wire-format error response with a plain-text body.
///
/// Used by [`dispatch_from_bytes`] for malformed input and by the
/// JNI bridge for panic fallback.  The response always carries
/// `content-type: text/plain; charset=utf-8`.
#[must_use]
pub fn error_wire(status: u16, msg: &str) -> Vec<u8> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_owned(),
        HeaderValue::Single("text/plain; charset=utf-8".to_owned()),
    );
    let metadata = ResponseMetadata::current();
    let parts = (
        status,
        headers,
        Bytes::copy_from_slice(msg.as_bytes()),
        metadata,
    );
    to_wire_bytes(parts)
}

// ── Internal Helpers ─────────────────────────────────────────────────

type ResponseParts = (u16, BTreeMap<String, HeaderValue>, Bytes, ResponseMetadata);

/// Drive a [`Router`] with the supplied envelope fields and return
/// raw response parts.
///
/// Returns `Err((status, msg))` only for pre-dispatch errors
/// (currently only "invalid HTTP method" → 405).  Router/handler
/// errors cannot occur because axum routers are
/// `Service<_, Error = Infallible>`.
async fn dispatch_parts(
    router: Router,
    method_str: &str,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body_bytes: Bytes,
) -> Result<ResponseParts, (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };

    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };

    // Case-insensitive Content-Type detection (RFC 7230 §3.2).
    let has_content_type = headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));

    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !body_bytes.is_empty() && !has_content_type {
        builder = builder.header("content-type", "application/json");
    }

    let request = builder
        .body(Body::from(body_bytes))
        .expect("request construction should not fail with valid URI");

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    Ok(collect_response_parts(response).await)
}

/// Drive a [`Router`] and stream response body chunks through
/// `on_chunk`, returning the status/headers/metadata once the body
/// stream finishes.
///
/// Same pre-dispatch error semantics as [`dispatch_parts`] (invalid
/// HTTP method → `Err((405, ...))`).  Body stream errors are silently
/// ended (the consumer sees a truncated response) because they
/// indicate the upstream handler aborted; the headers/status that
/// were already collected remain accurate.
async fn dispatch_response_streaming<F>(
    router: Router,
    method_str: &str,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body_bytes: Bytes,
    on_chunk: &mut F,
) -> Result<(u16, BTreeMap<String, HeaderValue>, ResponseMetadata), (u16, String)>
where
    F: FnMut(&[u8]),
{
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };

    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };

    let has_content_type = headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));

    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !body_bytes.is_empty() && !has_content_type {
        builder = builder.header("content-type", "application/json");
    }

    let request = builder
        .body(Body::from(body_bytes))
        .expect("request construction should not fail with valid URI");

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    let status = response.status().as_u16();

    let resp_headers = collect_header_map(response.headers());

    // Stream body chunks: pull frames one at a time and surface only
    // data frames (trailers are dropped — wire format does not carry
    // them).  Frame errors or end-of-stream both terminate cleanly.
    let mut body = response.into_body();
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            on_chunk(data.as_ref());
        }
    }

    Ok((status, resp_headers, ResponseMetadata::current()))
}

/// Collapse an [`http::HeaderMap`] into the wire's name → value map.
/// Headers with repeated names (e.g. `set-cookie`) are preserved as
/// [`HeaderValue::Multi`] so their semantics survive the conversion.
fn collect_header_map(headers: &http::HeaderMap) -> BTreeMap<String, HeaderValue> {
    let mut resp_headers: BTreeMap<String, HeaderValue> = BTreeMap::new();
    for (name, value) in headers {
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
    resp_headers
}

/// Collect status, headers, body bytes, and metadata from an axum
/// response.  Headers with repeated names are collapsed into
/// [`HeaderValue::Multi`] so semantics (e.g. `set-cookie`) are
/// preserved.
async fn collect_response_parts(response: axum::response::Response) -> ResponseParts {
    let status = response.status().as_u16();

    let resp_headers = collect_header_map(response.headers());

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();

    (
        status,
        resp_headers,
        body_bytes,
        ResponseMetadata::current(),
    )
}

/// Adapter: response parts → text envelope.  Non-UTF-8 bodies become
/// the empty string.
fn to_response_envelope_text(parts: ResponseParts) -> ResponseEnvelope {
    let (status, headers, body_bytes, metadata) = parts;
    let body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
    ResponseEnvelope {
        status,
        headers,
        body,
        metadata,
    }
}

/// Adapter: response parts → wire-format bytes.  Layout:
/// `[u32 BE header_len | JSON header | raw body]`.
///
/// For `status == 422` JSON responses we **best-effort** hoist any
/// `{"errors": [...]}` payload into the wire header's
/// `validation_errors` field — Java decoders can read validation
/// failures with a single header parse, while the original body is
/// preserved verbatim for clients that still rely on it.
fn to_wire_bytes(parts: ResponseParts) -> Vec<u8> {
    let (status, headers, body_bytes, metadata) = parts;
    let validation_errors = if status == 422 {
        try_hoist_validation_errors(&headers, &body_bytes)
    } else {
        None
    };
    let header = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers: &headers,
        metadata: &metadata,
        validation_errors,
    };
    let header_json =
        serde_json::to_vec(&header).expect("WireResponseHeader serialization is infallible");
    let header_len =
        u32::try_from(header_json.len()).expect("response header JSON exceeds u32::MAX bytes");
    let mut out = Vec::with_capacity(4 + header_json.len() + body_bytes.len());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_json);
    out.extend_from_slice(&body_bytes);
    out
}

/// Dispatch a request and split the response into
/// `(status, headers, metadata, body)` — exposing `axum::body::Body`
/// so callers can stream it themselves (vs. collecting it eagerly).
///
/// Used by the `*_with_header` streaming variants which need to emit
/// the wire-format header **before** body bytes start flowing.
async fn dispatch_and_split(
    router: Router,
    method_str: &str,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Body,
) -> Result<(u16, BTreeMap<String, HeaderValue>, ResponseMetadata, Body), (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };

    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };

    let mut builder = Request::builder().method(http_method).uri(&uri);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    let request = builder
        .body(body)
        .expect("request construction should not fail with valid URI");

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    let status = response.status().as_u16();

    let resp_headers = collect_header_map(response.headers());

    let body = response.into_body();
    Ok((status, resp_headers, ResponseMetadata::current(), body))
}

/// Build wire-format header bytes (`[u32 BE header_len | JSON header]`)
/// without a body — used by the `*_with_header` callback variants.
fn build_wire_header_bytes(
    status: u16,
    headers: &BTreeMap<String, HeaderValue>,
    metadata: &ResponseMetadata,
) -> Vec<u8> {
    let view = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers,
        metadata,
        validation_errors: None,
    };
    let header_json =
        serde_json::to_vec(&view).expect("WireResponseHeader serialization is infallible");
    let header_len =
        u32::try_from(header_json.len()).expect("response header JSON exceeds u32::MAX bytes");
    let mut out = Vec::with_capacity(4 + header_json.len());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_json);
    out
}

/// **Streaming dispatch with explicit header callback** — emits the
/// wire-format response header via `on_header` **before** any body
/// chunk is delivered to `on_chunk`.
///
/// This is the variant Spring `HttpServletResponse`-based controllers
/// want: `on_header` fires while the response is still uncommitted,
/// so the controller can call `resp.setStatus(...)` /
/// `resp.setHeader(...)` from the callback. Then `on_chunk` streams
/// the body bytes one frame at a time.
///
/// `on_header` is called **exactly once** in every code path —
/// success or error. On error (malformed wire, no app, invalid
/// method, …) the bytes passed to `on_header` are a normal
/// `error_wire(...)` response and `on_chunk` is **not** invoked.
pub async fn dispatch_streaming_with_header_async<H, F>(
    input: Vec<u8>,
    mut on_header: H,
    mut on_chunk: F,
) where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]),
{
    let (header, body_bytes) = match parse_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    if header.v != WIRE_VERSION {
        on_header(&error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        ));
        return;
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => {
            on_header(&wire);
            return;
        }
    };

    let (status, headers, metadata, mut body) = match dispatch_and_split(
        router,
        &header.method,
        header.path,
        header.query,
        header.headers,
        Body::from(body_bytes),
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            on_header(&error_wire(status, &msg));
            return;
        }
    };

    on_header(&build_wire_header_bytes(status, &headers, &metadata));

    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            on_chunk(data.as_ref());
        }
    }
}

/// Best-effort extract validation errors from a 422 JSON body.
///
/// Returns `None` (silently) for:
/// - non-JSON content-types (anything that doesn't end in `/json` or
///   `+json`)
/// - body bytes that don't parse as JSON
/// - JSON without an `errors` array, or with an empty array
///
/// This is intentionally lenient — a malformed 422 body must never
/// degrade to a 5xx; the original body is still surfaced verbatim.
fn try_hoist_validation_errors(
    headers: &BTreeMap<String, HeaderValue>,
    body_bytes: &Bytes,
) -> Option<Vec<ValidationErrorItem>> {
    let is_json = headers.iter().any(|(k, v)| {
        if !k.eq_ignore_ascii_case("content-type") {
            return false;
        }
        let s = match v {
            HeaderValue::Single(s) => s.as_str(),
            HeaderValue::Multi(vs) => vs.first().map_or("", String::as_str),
        };
        let mime = s
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        mime == "application/json" || mime.ends_with("+json")
    });
    if !is_json {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(body_bytes).ok()?;
    let errors = parsed.get("errors")?.as_array()?;
    let items: Vec<ValidationErrorItem> = errors
        .iter()
        .filter_map(|e| {
            let path = e.get("path")?.as_str()?.to_owned();
            let code = e
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let message = e
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Some(ValidationErrorItem {
                path,
                code,
                message,
            })
        })
        .collect();
    if items.is_empty() { None } else { Some(items) }
}

/// **Bidirectional streaming dispatch** — both request and response
/// bodies are streamed chunk-by-chunk; neither side materialises the
/// full payload in memory.
///
/// - `input_header` is a wire-format request **without a body**
///   (just `[u32 BE header_len | JSON header]`).  Send the body
///   chunks via `pull_chunk`, not embedded in this buffer.
/// - `pull_chunk` is called repeatedly to obtain request body
///   chunks.  Return `Some(chunk)` for each chunk and `None` to
///   signal EOF.  An empty `Some(Vec::new())` is treated as
///   "no more data right now, but keep the stream open" — rarely
///   useful; most callers should just return `None`.
/// - `on_chunk` receives response body chunks in arrival order, same
///   contract as [`dispatch_streaming_async`].
///
/// Returns the wire-format **header only** (`[u32 BE header_len |
/// header JSON]`) — the response body was delivered via `on_chunk`.
///
/// `pull_chunk` runs on a Tokio blocking thread (`spawn_blocking`)
/// because the JNI implementation reads from a Java `InputStream`,
/// which is inherently blocking.  Backpressure is enforced by a
/// bounded 16-slot mpsc channel: if axum reads slowly, the
/// `pull_chunk` call blocks naturally.
///
/// Failure modes match [`dispatch_streaming_async`]: malformed
/// header / unknown version / no app / handler error → normal
/// `error_wire(...)` response (with the message inside the returned
/// bytes); neither callback is invoked in those paths.
pub async fn dispatch_bidirectional_streaming<P, F>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
) -> Vec<u8>
where
    P: FnMut() -> Option<Vec<u8>> + Send + 'static,
    F: FnMut(&[u8]),
{
    let mut header_bytes: Vec<u8> = Vec::new();
    {
        let on_header = |h: &[u8]| header_bytes.extend_from_slice(h);
        bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header).await;
    }
    header_bytes
}

/// **Bidirectional streaming with explicit header callback** — the
/// `with_header` counterpart of [`dispatch_bidirectional_streaming`].
/// Emits the wire-format response header via `on_header` **before**
/// any response body byte reaches `on_chunk`, so Spring-style
/// `HttpServletResponse` controllers can commit status / headers
/// from the callback while the response is still uncommitted.
///
/// `on_header` is called exactly once on every code path (success or
/// error). On any pre-dispatch / wire error the bytes passed to
/// `on_header` are a normal `error_wire(...)` response and neither
/// `pull_chunk` nor `on_chunk` is invoked beyond that point.
pub async fn dispatch_bidirectional_streaming_with_header<P, F, H>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
    on_header: H,
) where
    P: FnMut() -> Option<Vec<u8>> + Send + 'static,
    F: FnMut(&[u8]),
    H: FnMut(&[u8]),
{
    bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header).await;
}

async fn bidirectional_streaming_inner<P, F, H>(
    input_header: Vec<u8>,
    pull_chunk: P,
    mut on_chunk: F,
    mut on_header: H,
) where
    P: FnMut() -> Option<Vec<u8>> + Send + 'static,
    F: FnMut(&[u8]),
    H: FnMut(&[u8]),
{
    let (header, _ignored_body) = match parse_wire_request(input_header) {
        Ok(parts) => parts,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    if header.v != WIRE_VERSION {
        on_header(&error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        ));
        return;
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => {
            on_header(&wire);
            return;
        }
    };

    // Bounded 16-slot mpsc — gives natural backpressure between the
    // pull_chunk producer thread and the axum handler consumer.
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(16);

    let producer_handle = tokio::task::spawn_blocking(move || {
        let mut pull = pull_chunk;
        // `None` from `pull()` ends the stream; an empty `Some(_)` is
        // skipped (it's not EOF); a failed `blocking_send` means the
        // receiver — axum's request body — was dropped because the
        // handler aborted mid-stream, so we stop pulling.
        while let Some(chunk) = pull() {
            if chunk.is_empty() {
                continue;
            }
            if tx.blocking_send(Bytes::from(chunk)).is_err() {
                break;
            }
        }
        // tx dropped at end of scope → axum sees end-of-stream.
    });

    let body = Body::new(ChannelBody { rx });
    let (status, headers, metadata, mut response_body) = match dispatch_and_split(
        router,
        &header.method,
        header.path,
        header.query,
        header.headers,
        body,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            let _ = producer_handle.await;
            on_header(&error_wire(status, &msg));
            return;
        }
    };

    on_header(&build_wire_header_bytes(status, &headers, &metadata));

    while let Some(Ok(frame)) = response_body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            on_chunk(data.as_ref());
        }
    }

    let _ = producer_handle.await;
}

/// Minimal `http_body::Body` implementation backed by an mpsc
/// `Receiver<Bytes>` — used by [`dispatch_bidirectional_streaming`]
/// to feed request body chunks into axum.
struct ChannelBody {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
}

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Parse a wire-format request.  On success returns the deserialised
/// header and the owned body bytes.
///
/// The body is split off as [`Bytes`] — a true zero-copy O(1)
/// refcount split of the input buffer (unlike `Vec::split_off`,
/// which allocates a new vector and memcpys the tail).
fn parse_wire_request(input: Vec<u8>) -> Result<(WireRequestHeader, Bytes), String> {
    if input.len() < 4 {
        return Err(format!(
            "wire input too short: {} bytes, need at least 4",
            input.len()
        ));
    }
    let mut input = Bytes::from(input);
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&input[..4]);
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    let total_header_end = 4usize.saturating_add(header_len);
    if total_header_end > input.len() {
        return Err(format!(
            "wire header_len ({header_len}) exceeds remaining input ({} bytes)",
            input.len() - 4
        ));
    }
    // O(1) split: both halves share the original allocation.
    let body = input.split_off(total_header_end);
    let header_json = &input[4..total_header_end];
    let header: WireRequestHeader = serde_json::from_slice(header_json)
        .map_err(|e| format!("wire header JSON parse error: {e}"))?;
    Ok((header, body))
}

#[cfg(test)]
mod wire_parse_tests {
    use super::parse_wire_request;

    /// Pins the zero-copy contract: the returned body must point into
    /// the original input allocation (no memcpy of the tail).
    #[test]
    fn parse_wire_request_body_is_zero_copy() {
        let header = br#"{"v":1,"method":"POST","path":"/x"}"#;
        let body = vec![0xABu8; 1024];
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(header);
        wire.extend_from_slice(&body);

        let input_ptr = wire.as_ptr() as usize;
        let body_offset = 4 + header.len();
        let (_, parsed_body) = parse_wire_request(wire).expect("valid wire request");

        assert_eq!(parsed_body.len(), 1024);
        assert_eq!(
            parsed_body.as_ptr() as usize,
            input_ptr + body_offset,
            "body must alias the original input buffer (zero-copy)"
        );
    }
}
