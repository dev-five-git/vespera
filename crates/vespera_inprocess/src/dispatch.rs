//! Public dispatch entry points: the direct (text envelope) API, the
//! binary wire API, and the direct-write (caller buffer) API.

use std::collections::BTreeMap;

use axum::body::Body;
use bytes::Bytes;
use http_body_util::BodyExt;

use crate::Router;
use crate::envelope::{RequestEnvelope, ResponseEnvelope, ResponseMetadata};
use crate::internal::{dispatch_and_split, dispatch_parts, to_response_envelope_text};
use crate::registry::resolve_app_router;
use crate::wire::{
    WIRE_VERSION, error_wire, parse_wire_header, split_wire_borrowed, split_wire_request,
    to_wire_bytes, write_wire_header_into_slice,
};

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
    let RequestEnvelope {
        method,
        path,
        query,
        headers,
        body,
    } = envelope;
    let parts = match dispatch_parts(
        router,
        &method,
        &path,
        &query,
        headers.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        Bytes::from(body),
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

/// Async sibling of [`dispatch_from_bytes`].  Use this when the caller
/// is already inside a Tokio runtime (e.g. an axum handler embedding
/// another vespera router, or a tokio-spawned task in the JNI bridge's
/// async dispatch path).
///
/// All failure modes return a valid wire-format response (same
/// guarantees as [`dispatch_from_bytes`]), including `404` when no app
/// is registered under the requested name.
pub async fn dispatch_from_bytes_async(input: Vec<u8>) -> Vec<u8> {
    // Ingress cap (defense-in-depth): reject an oversized buffered
    // request with 413 before doing any further work.  Unlimited by
    // default (see `max_request_bytes`); streaming paths are exempt.
    if crate::config::request_exceeds_limit(input.len()) {
        return error_wire(
            413,
            &format!(
                "request size {} bytes exceeds configured maximum of {} bytes",
                input.len(),
                crate::config::max_request_bytes()
            ),
        );
    }
    // Wire-level checks next: malformed input must report parse
    // errors regardless of whether an app is registered.
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return error_wire(400, &msg),
    };
    let header = match parse_wire_header(&header_bytes) {
        Ok(h) => h,
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
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
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
    // Ingress cap (defense-in-depth) — same policy as
    // `dispatch_from_bytes_async`; 413 written into the caller buffer.
    if crate::config::request_exceeds_limit(input.len()) {
        return write_wire_into(
            out,
            &error_wire(
                413,
                &format!(
                    "request size {} bytes exceeds configured maximum of {} bytes",
                    input.len(),
                    crate::config::max_request_bytes()
                ),
            ),
        );
    }
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return write_wire_into(out, &error_wire(400, &msg)),
    };
    let header = match parse_wire_header(&header_bytes) {
        Ok(h) => h,
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

    // Mirror dispatch_parts' Content-Type defaulting (body present, no
    // content-type → application/json) so the direct-write path is
    // request-compatible with dispatch_from_bytes.  The body's
    // emptiness is known here (unlike the streaming callers), so the
    // default is applied on the request builder — no map insert, no
    // String allocations.
    let default_json_content_type = !body_bytes.is_empty()
        && !header
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));

    let (status, headers, metadata, body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        Body::from(body_bytes),
        default_json_content_type,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return write_wire_into(out, &error_wire(status, &msg)),
    };

    finish_direct_write(out, status, headers, metadata, body).await
}

/// Dispatch a wire request from a **borrowed** input slice, writing the
/// wire response directly into `out` — the zero-input-copy sibling of
/// [`dispatch_into_async`].
///
/// Where [`dispatch_into_async`] takes an owned `Vec<u8>`, this borrows
/// `input` for the whole call: the wire header is parsed **in place** (no
/// copy) and only the request **body** region is copied into an owned
/// [`Bytes`] (axum's `Body` requires `'static` ownership).  A **bodyless**
/// request — the common DIRECT `GET` — therefore copies nothing at all,
/// and any request saves the header-region copy `dispatch_into_async`'s
/// owned `Vec` pays.
///
/// # Safety / lifetime
///
/// The returned future borrows `input`; the caller MUST keep `input`
/// valid until the future completes.  The JNI direct-buffer caller
/// satisfies this by pinning the source `ByteBuffer` (a live local ref)
/// for the entire `block_on`.
///
/// Byte-identical to [`dispatch_into_async`] for the same wire bytes; all
/// the same error / `422` / overflow semantics apply.
pub async fn dispatch_into_async_borrowed(input: &[u8], out: &mut [u8]) -> DirectWriteResult {
    // Ingress cap (defense-in-depth) — same policy as `dispatch_into_async`.
    if crate::config::request_exceeds_limit(input.len()) {
        return write_wire_into(
            out,
            &error_wire(
                413,
                &format!(
                    "request size {} bytes exceeds configured maximum of {} bytes",
                    input.len(),
                    crate::config::max_request_bytes()
                ),
            ),
        );
    }
    let (header_bytes, body_bytes) = match split_wire_borrowed(input) {
        Ok(parts) => parts,
        Err(msg) => return write_wire_into(out, &error_wire(400, &msg)),
    };
    let header = match parse_wire_header(header_bytes) {
        Ok(h) => h,
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

    let default_json_content_type = !body_bytes.is_empty()
        && !header
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));

    // Borrowed path: the header is parsed in place (borrowing `input`);
    // only the body region is copied into an owned `Bytes`.  An empty
    // body (the common bodyless GET) allocates nothing.
    let body = if body_bytes.is_empty() {
        Body::empty()
    } else {
        Body::from(Bytes::copy_from_slice(body_bytes))
    };

    let (status, headers, metadata, resp_body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        body,
        default_json_content_type,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return write_wire_into(out, &error_wire(status, &msg)),
    };

    finish_direct_write(out, status, headers, metadata, resp_body).await
}

/// Shared tail of the direct-write dispatchers ([`dispatch_into_async`]
/// and [`dispatch_into_async_borrowed`]): `422` responses are materialised
/// so `validation_errors` hoisting is preserved byte-for-byte; every other
/// status streams status + headers + body frames straight into `out`,
/// reporting the exact required size on overflow.
async fn finish_direct_write(
    out: &mut [u8],
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    mut body: Body,
) -> DirectWriteResult {
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

    // Write the wire header straight into `out` — no intermediate Vec
    // and no second copy.  `header_total` is the exact header byte count
    // whether or not it fit, so overflow reporting stays exact.
    let header_total = write_wire_header_into_slice(out, status, &headers, &metadata);
    let mut written = if header_total <= out.len() {
        header_total
    } else {
        0
    };
    let mut required = header_total;

    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
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
            // Response body aborted mid-stream. Nothing has been committed to
            // the caller yet (we write into `out` and only return at the end),
            // so discard the partial write and emit a 500 error wire instead
            // of reporting truncated bytes as a successful response.
            Some(Err(_)) => {
                let wire = error_wire(500, "response body stream error");
                return write_wire_into(out, &wire);
            }
            None => break,
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
