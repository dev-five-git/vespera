//! Public dispatch entry points: the direct (text envelope) API, the
//! binary wire API, and the direct-write (caller buffer) API.

use std::collections::BTreeMap;

use axum::body::Body;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_body_util::BodyExt;

use crate::Router;
use crate::envelope::{RequestEnvelope, ResponseEnvelope, ResponseMetadata};
use crate::internal::{
    BODY_STREAM_ERROR_MSG, HEADER_TOO_LARGE_MSG, dispatch_and_split, dispatch_parts,
    to_response_envelope_text,
};
use crate::registry::resolve_app_router;
use crate::wire::{
    WIRE_VERSION, WireRequestHeader, error_wire, header_capacity_with_floor, parse_wire_header,
    split_wire_borrowed, split_wire_request, to_wire_bytes, write_wire_header_into_slice,
    write_wire_header_into_vec,
};

// ── Shared wire prelude (used by every wire entry point) ─────────────

/// Ingress-cap guard shared by the **buffered** wire entry points
/// (`dispatch_from_bytes_async`, `dispatch_into_async`,
/// `dispatch_into_async_borrowed`, and the response-streaming pair).
/// Returns the `413` wire bytes when the request exceeds the configured
/// maximum, else `None`.  Centralizing the message keeps the cap identical
/// across entry points; **bidirectional** streaming is intentionally exempt
/// (it is `O(chunk)` RAM) and so does not call this.
#[inline]
pub fn check_ingress_cap(len: usize) -> Option<Vec<u8>> {
    // Load the cap once and reuse it for both the predicate and the error
    // message — the two-call shape used to pay a second `OnceLock::get`
    // atomic load on the (rare) 413 path just to render the same value into
    // the message.  The comparison itself is `config::exceeds`, the single
    // spelling shared with `config::request_exceeds_limit`, so the two can
    // never drift.  The wire body is byte-identical to the prior positional
    // format; exercised by
    // `crates/vespera_inprocess/tests/request_size_cap.rs`.
    let max = crate::config::max_request_bytes();
    if crate::config::exceeds(len, max) {
        Some(error_wire(
            413,
            &format!("request size {len} bytes exceeds configured maximum of {max} bytes"),
        ))
    } else {
        None
    }
}

/// Two-step prelude shared by the **owned** wire entry points: enforce
/// [`check_ingress_cap`], then [`split_wire_request`] the input into its
/// header-JSON and body regions, mapping a split failure to the `400`
/// wire bytes.
///
/// The three owned-wire callers — [`dispatch_owned_to_parts`],
/// [`super::streaming::dispatch_streaming_async`] and
/// [`super::streaming::dispatch_streaming_with_header_async`] — each
/// repeated this identical pair, a drift hazard for the two things it
/// carries (the cap applies before any parsing work, and a split error
/// is a `400`, not a `413`).  Callers keep their own `Err` delivery
/// shape: return the bytes, or hand them to a header callback.
///
/// NOT used by bidirectional streaming
/// ([`super::streaming::dispatch_bidirectional_streaming`] and friends):
/// it deliberately skips the ingress cap because it runs in `O(chunk)`
/// RAM — see the exemption documented on [`check_ingress_cap`].  The
/// borrowed direct-write entry point is likewise not a caller: it splits
/// with `split_wire_borrowed`, not `split_wire_request`.
///
/// `#[inline]` keeps codegen identical to the prior inlined shape.
#[inline]
pub fn cap_and_split(input: Vec<u8>) -> Result<(Bytes, Bytes), Vec<u8>> {
    if let Some(err) = check_ingress_cap(input.len()) {
        return Err(err);
    }
    split_wire_request(input).map_err(|msg| error_wire(400, &msg))
}

/// Wire-prelude shared by **every** wire entry point (buffered,
/// direct-write, and streaming): parse the header, enforce the protocol
/// [`WIRE_VERSION`], and resolve the target app [`Router`].  Centralizing
/// this keeps the security-sensitive version check + app resolution
/// byte-identical across all dispatchers — the previous per-entry-point
/// copies were a drift hazard.
///
/// `header_bytes` is the wire header-JSON region; the returned
/// [`WireRequestHeader`] borrows from it, so the caller MUST keep it alive
/// for as long as the header is used.  On failure the `Err` carries the
/// exact wire error bytes to deliver in the caller's shape (`400` for a
/// parse error or version mismatch, `400`/`404` from app resolution).
#[inline]
pub fn parse_validate_resolve(
    header_bytes: &[u8],
) -> Result<(WireRequestHeader<'_>, Router), Vec<u8>> {
    let header = parse_wire_header(header_bytes).map_err(|msg| error_wire(400, &msg))?;
    if header.v != WIRE_VERSION {
        return Err(error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        ));
    }
    let router = resolve_app_router(&header)?;
    Ok((header, router))
}

/// Return the sub-[`Bytes`] of `owner` that exactly backs the string slice `s`,
/// or `None` when `s` does not lie within `owner`.
///
/// Used on the OWNED wire path to build a zero-copy [`http::Uri`] from the
/// request's owning header `Bytes` — sharing the bytes `Uri::try_from(&str)`
/// would otherwise re-allocate and copy.  The pointer arithmetic is fully
/// checked, and because distinct heap allocations never overlap, an `s` that
/// lives in its OWN allocation (an escaped `Cow::Owned` path, or a borrowed
/// string from a different buffer) can never satisfy the in-range bound: the
/// function returns `None` and the caller falls back to the copying path.  So a
/// returned `Some(bytes)` is guaranteed to hold exactly `s`'s bytes — there is
/// no provenance-confusion path, and `slice` itself never panics.
///
/// `pub` (inside this private `dispatch` module, so reachable only crate-side)
/// so [`super::internal`] can share the same provenance check when it builds
/// zero-copy `HeaderValue`s from wire-borrowed value strings.
#[inline]
pub fn slice_from_owner(owner: &Bytes, s: &str) -> Option<Bytes> {
    let base = owner.as_ptr() as usize;
    let off = (s.as_ptr() as usize).checked_sub(base)?;
    let end = off.checked_add(s.len())?;
    (end <= owner.len()).then(|| owner.slice(off..end))
}

/// Compute the zero-copy URI fast-path `path_bytes` for an **owned** wire
/// entry point: when the request has no query and the parsed path borrows
/// from the owning `header_bytes`, return the sub-`Bytes` covering it so
/// `dispatch_and_split` can build the [`http::Uri`] via
/// `Uri::from_maybe_shared` instead of paying the `Uri::try_from(&str)` copy.
/// Any non-empty query or any escaped/owned path returns `None` and the
/// caller falls back to the copying `build_uri` path.
///
/// Sole call site: [`dispatch_owned_to_parts`], the shared prelude that
/// both public owned-wire entry points — [`dispatch_from_bytes_async`]
/// and [`dispatch_into_async`] — reach on every dispatch (they diverge
/// only in tail-delivery shape: buffered `Vec` vs direct-write
/// `&mut [u8]`).  Centralising the URI fast-path here means the two
/// public dispatchers keep an identical owned-wire URI policy without a
/// second copy of the logic to drift.  The borrowed entry point
/// ([`dispatch_into_async_borrowed`]) is deliberately *not* a caller: it
/// has no owning `Bytes` to share and so unconditionally passes `None`.
/// `#[inline]` keeps codegen identical to the prior copy-pasted shape.
#[inline]
fn path_bytes_for_owned(header_bytes: &Bytes, header: &WireRequestHeader<'_>) -> Option<Bytes> {
    if header.query.is_empty() {
        slice_from_owner(header_bytes, &header.path)
    } else {
        None
    }
}

/// Shared prelude of the owned-wire dispatchers
/// ([`dispatch_from_bytes_async`] and [`dispatch_into_async`]):
/// ingress cap → wire split → parse/validate/resolve → zero-copy URI
/// fast-path → [`dispatch_and_split`].  Returns the dispatched response
/// parts on success, or ready-to-deliver wire error bytes (`error_wire`
/// shape) on any pre-dispatch failure.
///
/// Centralising the 6-step prelude eliminates a real drift hazard: the
/// two owned-wire entry points previously inlined ~45 lines of
/// structurally identical code carrying many subtle invariants
/// (zero-copy URI fast-path, header-bytes owner threading, Content-Type
/// defaulting), and a one-sided edit (e.g. inserting a pre-dispatch
/// hook, adjusting `default_json_when_absent` semantics, swapping the
/// URI fast-path) would silently desynchronise them.  The borrowed
/// sibling [`dispatch_into_async_borrowed`] is intentionally NOT a
/// third caller — it uses `split_wire_borrowed`, has no owning `Bytes`
/// to feed `path_bytes_for_owned`, and copies only the body region.
///
/// Caller tail-delivery shape (buffered `Vec` vs direct-write
/// `&mut [u8]`) stays at the call site: each caller routes `Ok` to its
/// finisher and `Err` to its appropriate sink (return directly /
/// `write_wire_into`).  The helper has a single caller per use, so it
/// inlines cleanly and adds no synthetic await point — byte-identical
/// to the prior copy-pasted shape.
async fn dispatch_owned_to_parts(
    input: Vec<u8>,
) -> Result<(u16, http::HeaderMap, ResponseMetadata, Body), Vec<u8>> {
    // Ingress cap (defense-in-depth, 413) then wire split (400).  Malformed
    // input must report parse errors regardless of whether an app is
    // registered, so the split happens first and the shared
    // parse/version/resolve follows.  See [`cap_and_split`].
    let (header_bytes, body_bytes) = cap_and_split(input)?;
    let (header, router) = parse_validate_resolve(&header_bytes)?;

    // Content-Type defaulting (non-empty body with no explicit
    // content-type → application/json) is applied inside dispatch_and_split,
    // which detects the header during its build pass; we only signal that a
    // non-empty body should default.  Computed before `body_bytes` is moved.
    let default_json_when_absent = !body_bytes.is_empty();

    // Owned path: with no query and a path borrowed from the owning header
    // `Bytes`, hand its sub-`Bytes` to the URI builder so the URI SHARES those
    // bytes instead of `Uri::try_from(&str)` copying them — one fewer
    // per-request allocation (`path_bytes_for_owned` / `slice_from_owner` /
    // `dispatch_and_split`).
    let path_bytes = path_bytes_for_owned(&header_bytes, &header);

    dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        path_bytes,
        header.iter_str_pairs(),
        // Owned wire path: the parsed header values borrow from
        // `header_bytes` (plain values are `Cow::Borrowed` straight out of
        // the wire input).  Pass the owning `Bytes` so each `HeaderValue`
        // is constructed zero-copy via `HeaderValue::from_maybe_shared`
        // (escaped `Cow::Owned` values cleanly miss the in-range bound
        // check and fall back to the copy path).
        Some(&header_bytes),
        Body::from(body_bytes),
        default_json_when_absent,
    )
    .await
    .map_err(|(status, msg)| error_wire(status, &msg))
}

// ── Dispatch (direct API — backward compatible) ──────────────────────

/// Hand-rendered `500` [`ResponseEnvelope`] JSON used by [`dispatch`] when
/// `serde_json::to_string` fails (unreachable in practice — see the call
/// site).
///
/// COUPLING: these bytes mirror `ResponseEnvelope`'s serde shape
/// field-for-field (`status`, `headers`, `body`, `metadata.version`), and
/// the nested object mirrors [`ResponseMetadata`].  Adding or renaming a
/// serialized field on either type (`envelope.rs`) without updating this
/// literal breaks the byte-identity guard
/// [`tests::fallback_matches_serde_serialization`] below — that test is the
/// drift tripwire, so keep the two in lockstep.  Mirrors the same discipline
/// `crate::wire::header_write::write_response_header` documents against
/// `hand_serialize_matches_serde_serialize`.
const ENVELOPE_SERIALIZATION_FALLBACK: &str = concat!(
    r#"{"status":500,"headers":{},"body":"envelope serialization failed","#,
    r#""metadata":{"version":""#,
    env!("CARGO_PKG_VERSION"),
    r#""}}"#,
);

/// Dispatch a [`RequestEnvelope`] through an axum [`Router`] and
/// return the serialised [`ResponseEnvelope`] JSON.
///
/// This borrows the envelope and clones its owned fields before
/// passing them to the hot path.  Callers that already own a
/// [`RequestEnvelope`] should prefer [`dispatch_owned`] to skip the
/// clone.
pub async fn dispatch(router: Router, envelope: &RequestEnvelope) -> String {
    let result = dispatch_owned(router, envelope.clone()).await;
    serde_json::to_string(&result).unwrap_or_else(|_| {
        // Unreachable in practice: `ResponseEnvelope` derives `Serialize`
        // over only primitives, `String`, `BTreeMap`, and `Cow<'static, str>`
        // — none of which can `Err` in `serde_json`.  A hand-rendered
        // byte-identical `500` envelope keeps this public direct-API entry
        // free of unwind sites (matching the no-panic/unwind discipline the
        // FFI-adjacent hot path documents in
        // [`crate::internal::router_oneshot`],
        // [`crate::wire::header_write`], and
        // [`crate::wire::header_read::expect_literal`]), while preserving
        // the same JSON shape (`status`/`headers`/`body`/`metadata.version`)
        // the derived path emits so external decoders are unaffected.
        String::from(ENVELOPE_SERIALIZATION_FALLBACK)
    })
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
        // Envelope dispatch: `headers` are owned `String`s with no wire
        // owner, so the zero-copy fast path does not apply — pass `None`
        // and let `header_value_from_owner` fall back to `from_str` (same
        // bytes, same errors, no behaviour change).
        None,
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
    // Shared owned-wire prelude: ingress cap → split → parse/validate/resolve
    // → zero-copy URI fast-path → dispatch_and_split.  Centralised in
    // [`dispatch_owned_to_parts`] so the two owned-wire dispatchers cannot
    // drift; this caller's tail just routes Ok to `finish_buffered_wire` and
    // Err to the buffered shape (the wire bytes go straight back to the
    // caller).
    match dispatch_owned_to_parts(input).await {
        Ok((status, headers, metadata, body)) => {
            finish_buffered_wire(status, headers, metadata, body).await
        }
        Err(wire) => wire,
    }
}

/// Materialise a `422 Unprocessable Entity` response into wire bytes so the
/// `validation_errors` hoisting into the wire header (performed inside
/// [`to_wire_bytes`]) is preserved **byte-for-byte** — validation failures
/// are tiny + cold so the materialise cost is in the noise, and the hoist
/// invariant is non-negotiable (Java decoders rely on it).
///
/// On a body-stream error mid-collect, returns `error_wire(500, ...)`
/// instead of falling through to an empty-bodied 422 — a truncated/failed
/// response must never be reported as a clean success.  This matches the
/// non-422 paths in [`finish_buffered_wire`] / [`finish_direct_write`] and
/// the [`crate::internal::collect_response_parts`] contract.
///
/// Shared by both finish_* tails so the [`BODY_STREAM_ERROR_MSG`] string +
/// the 422 hoist-preservation invariant live in exactly one place.
async fn build_422_wire(
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    body: Body,
) -> Vec<u8> {
    let Ok(collected) = body.collect().await else {
        // Body aborted mid-collect: a failed 422 must surface as a 500,
        // never as a clean (empty-bodied) 422 — same contract as the
        // non-422 path and `collect_response_parts`.
        return error_wire(500, BODY_STREAM_ERROR_MSG);
    };
    let body_bytes = collected.to_bytes();
    to_wire_bytes((status, headers, body_bytes, metadata))
}

/// Buffered sibling of [`finish_direct_write`]: assemble the full wire
/// response `Vec` by streaming the response body **frames straight into
/// the final buffer**, instead of collecting the body into an intermediate
/// `Bytes` (via `http_body_util::Collected::to_bytes`) and copying it again
/// in [`to_wire_bytes`].
///
/// * Single-frame body (the common `Json`/`Bytes`/`String` response): the
///   emitted bytes and the single body copy are identical to the previous
///   `collect()` + `to_wire_bytes` path, minus the `Collected` / `to_bytes`
///   layer.
/// * Multi-frame body: also removes the `to_bytes` concatenation copy and
///   keeps peak memory at ~one body (the growing `Vec`) instead of
///   body-plus-collected.
///
/// `status == 422` is routed through [`build_422_wire`] to preserve the
/// `validation_errors` hoisting byte-for-byte; a body-stream error mid-drain
/// on the non-422 path discards the partial buffer and returns
/// `error_wire(500, ...)`, matching the previous
/// [`crate::internal::dispatch_parts`] 500-on-body-error contract.
async fn finish_buffered_wire(
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    mut body: Body,
) -> Vec<u8> {
    if status == 422 {
        return build_422_wire(status, headers, metadata, body).await;
    }

    // Size the final buffer up front: 4-byte length prefix + adaptive header
    // estimate (floored at WIRE_HEADER_RESERVE so small-header responses
    // never reserve less than before) + the body's exact size when the body
    // reports one (Full bodies do), so a single-frame response serializes
    // with zero reallocations.
    let header_cap = header_capacity_with_floor(&headers, &metadata);
    let body_cap = usize::try_from(body.size_hint().exact().unwrap_or(0)).unwrap_or(0);
    // Saturating so a pathological/oversized exact body hint cannot wrap the
    // capacity arithmetic (debug panic / release wrap → under-reserve); the
    // common case computes the identical value, and `finish_direct_write`
    // already uses the same saturating accounting for its overflow reporting.
    let mut out = Vec::with_capacity(4usize.saturating_add(header_cap).saturating_add(body_cap));
    if !write_wire_header_into_vec(&mut out, status, &headers, &metadata) {
        // Unreachable for a real `HeaderMap` (4 GiB+ of header JSON); never
        // panic on the response path — emit a 500 wire response instead.
        return error_wire(500, HEADER_TOO_LARGE_MSG);
    }

    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref()
                    && !data.is_empty()
                {
                    out.extend_from_slice(data);
                }
            }
            // Body aborted mid-stream: nothing has been handed to the caller
            // yet (we return only at the end), so discard the partial buffer
            // and emit a 500 rather than a truncated body — mirrors the
            // collect_response_parts 500-on-body-error contract.
            Some(Err(_)) => return error_wire(500, BODY_STREAM_ERROR_MSG),
            None => break,
        }
    }
    out
}

/// Outcome of [`dispatch_into_async`] / [`dispatch_into`].
///
/// Carries the truncation/overflow discriminant the FFI bridge relies on
/// to decide whether `out[..n]` is a complete wire response or the
/// response needs a larger buffer.  Dropping this value silently treats
/// `Overflow(required)` as `Complete(n)`, exposing the
/// read-uninitialised-prefix hazard documented on
/// [`DirectWriteResult::Overflow`] — hence `#[must_use]`.
#[must_use]
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
/// If `out` is too small the **exact** required size is reported via
/// [`DirectWriteResult::Overflow`].  An exact-length body (a `Full`
/// response / explicit `Content-Length`) reports it immediately from the
/// body's size hint **without draining**; an unknown-length (streaming)
/// body is drained (counting, not writing) to compute the size.  Either
/// way the handler has already run; retrying runs it again — callers must
/// gate retries on idempotency.
pub async fn dispatch_into_async(input: Vec<u8>, out: &mut [u8]) -> DirectWriteResult {
    // Shared owned-wire prelude lives in [`dispatch_owned_to_parts`] — same
    // ingress cap / split / parse-validate-resolve / zero-copy URI fast-path
    // as `dispatch_from_bytes_async`.  This caller's tail diverges only in
    // shape: Ok streams straight into `out` via `finish_direct_write`; Err is
    // the same wire bytes, written into `out` via `write_wire_into` instead
    // of returned directly.
    match dispatch_owned_to_parts(input).await {
        Ok((status, headers, metadata, body)) => {
            finish_direct_write(out, status, headers, metadata, body).await
        }
        Err(wire) => write_wire_into(out, &wire),
    }
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
    if let Some(err) = check_ingress_cap(input.len()) {
        return write_wire_into(out, &err);
    }
    let (header_bytes, body_bytes) = match split_wire_borrowed(input) {
        Ok(parts) => parts,
        Err(msg) => return write_wire_into(out, &error_wire(400, &msg)),
    };
    let (header, router) = match parse_validate_resolve(header_bytes) {
        Ok(parts) => parts,
        Err(wire) => return write_wire_into(out, &wire),
    };

    // dispatch_and_split detects Content-Type during its build pass; we
    // only signal that a non-empty body should default to JSON.
    let default_json_when_absent = !body_bytes.is_empty();

    // Borrowed path: the header is parsed in place (borrowing `input`);
    // only the body region is copied into an owned `Bytes`.  An empty
    // body (the common bodyless GET) allocates nothing.
    let body = if body_bytes.is_empty() {
        Body::empty()
    } else {
        Body::from(Bytes::copy_from_slice(body_bytes))
    };

    // Borrowed path: `input` is not owned, so there is no request-lifetime
    // `Bytes` to share into the URI — pass `None` and let `build_uri` parse the
    // borrowed path (the zero-copy URI win applies only to the owned paths).
    // Same reasoning for the header-owner argument: no owning `Bytes`, so
    // `header_value_from_owner` falls back to `from_str` exactly as before.
    let (status, headers, metadata, resp_body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        None,
        header.iter_str_pairs(),
        None,
        body,
        default_json_when_absent,
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
        // Materialise via the shared [`build_422_wire`] helper to preserve
        // the `validation_errors` hoisting in the wire header byte-for-byte
        // and to keep the [`BODY_STREAM_ERROR_MSG`] 500 fallback in one
        // place (see the helper's contract docs).
        let wire = build_422_wire(status, headers, metadata, body).await;
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

    // Fast overflow: when the body length is known exactly (a `Full` body /
    // explicit `Content-Length`) and the response cannot fit, report the
    // exact required size immediately instead of draining every frame just
    // to count bytes — this is the undersized-buffer retry path the pooled
    // JNI `dispatchDirect` takes. Unknown-length (streaming) bodies have no
    // exact hint and fall through to the drain loop unchanged.
    if let Some(exact) = body.size_hint().exact() {
        let required_u64 = u64::try_from(header_total)
            .unwrap_or(u64::MAX)
            .saturating_add(exact);
        if required_u64 > u64::try_from(out.len()).unwrap_or(u64::MAX) {
            return DirectWriteResult::Overflow(
                usize::try_from(required_u64).unwrap_or(usize::MAX),
            );
        }
    }

    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref()
                    && !data.is_empty()
                {
                    let len = data.len();
                    // Write only while the output is still contiguous
                    // (`written == required` ⇒ nothing has been skipped yet).
                    // `checked_add` guards the bounds test against a
                    // pathological frame length wrapping `usize`; `written`
                    // then stays ≤ `out.len()` so the in-place add cannot
                    // overflow.
                    if written == required
                        && written.checked_add(len).is_some_and(|end| end <= out.len())
                    {
                        out[written..written + len].copy_from_slice(data);
                        written += len;
                    }
                    // Saturating so an (impossible-in-practice) cumulative
                    // overflow reports `Overflow(usize::MAX)` rather than
                    // wrapping to a bogus small required size.
                    required = required.saturating_add(len);
                }
            }
            // Response body aborted mid-stream. Nothing has been committed to
            // the caller yet (we write into `out` and only return at the end),
            // so discard the partial write and emit a 500 error wire instead
            // of reporting truncated bytes as a successful response.
            Some(Err(_)) => {
                let wire = error_wire(500, BODY_STREAM_ERROR_MSG);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift tripwire for [`ENVELOPE_SERIALIZATION_FALLBACK`]: the
    /// hand-rendered `500` JSON must stay byte-identical to what the derived
    /// `Serialize` impl emits for the equivalent [`ResponseEnvelope`].
    ///
    /// Fails when a serialized field is added, removed, renamed, or reordered
    /// on [`ResponseEnvelope`] or [`ResponseMetadata`] (`envelope.rs`) without
    /// updating the literal in lockstep.
    #[test]
    fn fallback_matches_serde_serialization() {
        let derived = serde_json::to_string(&ResponseEnvelope {
            status: 500,
            headers: BTreeMap::new(),
            body: "envelope serialization failed".to_owned(),
            metadata: ResponseMetadata::current(),
        })
        .expect("ResponseEnvelope serialization cannot fail");

        assert_eq!(derived, ENVELOPE_SERIALIZATION_FALLBACK);
    }

    fn request_wire(header: &[u8]) -> Vec<u8> {
        let mut wire = u32::try_from(header.len())
            .expect("test header fits u32")
            .to_be_bytes()
            .to_vec();
        wire.extend_from_slice(header);
        wire
    }

    fn response_status(wire: &[u8]) -> u64 {
        let header_len = u32::from_be_bytes(wire[..4].try_into().expect("wire prefix")) as usize;
        let header: serde_json::Value =
            serde_json::from_slice(&wire[4..4 + header_len]).expect("response header JSON");
        header["status"].as_u64().expect("numeric response status")
    }

    #[tokio::test]
    async fn borrowed_direct_write_surfaces_parse_and_dispatch_errors() {
        crate::register_app_named("dispatch_unit_edges", crate::Router::new);
        let mut out = vec![0u8; 1024];

        let malformed = request_wire(br#"{"v":1,"method":}"#);
        let DirectWriteResult::Complete(n) =
            dispatch_into_async_borrowed(&malformed, &mut out).await
        else {
            panic!("malformed-input error wire must fit");
        };
        assert_eq!(response_status(&out[..n]), 400);

        let invalid_method = request_wire(
            br#"{"v":1,"method":"BAD METHOD","path":"/","app":"dispatch_unit_edges"}"#,
        );
        let DirectWriteResult::Complete(n) =
            dispatch_into_async_borrowed(&invalid_method, &mut out).await
        else {
            panic!("invalid-method error wire must fit");
        };
        assert_eq!(response_status(&out[..n]), 405);
    }

    #[test]
    fn assembled_wire_copy_reports_exact_overflow() {
        let mut out = [0u8; 2];
        assert_eq!(
            write_wire_into(&mut out, b"three"),
            DirectWriteResult::Overflow(5)
        );
    }
}
