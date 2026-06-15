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
//!    This API is intended for **in-process Rust embedding** where a
//!    typed envelope is convenient.  It is not the throughput-oriented
//!    path: the response headers are materialised into an owned
//!    `BTreeMap<String, HeaderValue>` and the body is decoded to a
//!    `String`.  **FFI / high-throughput callers should prefer the
//!    binary wire API** ([`dispatch_from_bytes`] / [`dispatch_into`]),
//!    which borrows the wire header, serialises response headers
//!    straight from the `http::HeaderMap`, and carries the body as raw
//!    bytes (no UTF-8 round-trip).  Within the direct API itself,
//!    prefer [`dispatch_owned`] over [`dispatch`] / [`dispatch_typed`]
//!    to avoid cloning the request envelope.
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

mod config;
mod dispatch;
mod envelope;
mod internal;
mod registry;
mod streaming;
mod wire;

/// Re-export `axum::Router` so consumers don't need a direct axum dependency.
pub use axum::Router;
pub use config::{
    DEFAULT_STREAMING_CHANNEL_CAPACITY, DEFAULT_STREAMING_CHUNK_BYTES, max_request_bytes,
    request_exceeds_limit, set_max_request_bytes, set_streaming_channel_capacity,
    set_streaming_chunk_bytes, streaming_channel_capacity, streaming_chunk_bytes,
};
pub use dispatch::{
    DirectWriteResult, dispatch, dispatch_from_bytes, dispatch_from_bytes_async, dispatch_into,
    dispatch_into_async, dispatch_owned, dispatch_typed,
};
pub use envelope::{
    HeaderValue, RequestEnvelope, ResponseEnvelope, ResponseMetadata, error_envelope,
};
pub use registry::{DEFAULT_APP_NAME, register_app, register_app_named};
pub use streaming::{
    RequestChunk, StreamAbort, dispatch_bidirectional_streaming,
    dispatch_bidirectional_streaming_closing, dispatch_bidirectional_streaming_with_header,
    dispatch_bidirectional_streaming_with_header_closing, dispatch_streaming_async,
    dispatch_streaming_with_header_async,
};
pub use wire::error_wire;
