//! Public dispatch entry points: the direct (text envelope) API, the
//! binary wire API, and the direct-write (caller buffer) API.

use std::collections::BTreeMap;

use axum::body::Body;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_body_util::BodyExt;

use crate::Router;
use crate::bounded::{
    Deadline, Prefetched422, append_capped, prefetch_422_body, response_alloc_failed_wire,
    response_too_large_wire, timeout_wire,
};
use crate::config::max_buffered_response_bytes;
use crate::direct::{
    DirectWriteResult, ResumableDirectWrite, finish_direct_write, finish_resumable,
    resumable_from_wire, write_wire_into,
};
use crate::envelope::{RequestEnvelope, ResponseEnvelope, ResponseMetadata};
use crate::internal::{
    BODY_STREAM_ERROR_MSG, HEADER_TOO_LARGE_MSG, dispatch_and_split, dispatch_parts,
    to_response_envelope_text,
};
use crate::registry::resolve_app_router;
use crate::wire::{
    MAX_REQUEST_HEADERS, TOO_MANY_HEADERS_MSG, WIRE_VERSION, WireRequestHeader, error_wire,
    header_capacity_with_floor, parse_wire_header, split_wire_borrowed, split_wire_request,
    to_wire_bytes, write_wire_header_into_vec,
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
    let header = parse_wire_header(header_bytes)
        .map_err(|msg| error_wire(parse_error_status(&msg), &msg))?;
    #[cfg(feature = "tracing")]
    crate::obs::record_request(&header.method, &header.path, header.app.as_deref());
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

/// `431` envelope for a [`dispatch_owned`] call with more than
/// [`MAX_REQUEST_HEADERS`] headers.
#[cold]
#[inline(never)]
fn too_many_headers_envelope() -> ResponseEnvelope {
    ResponseEnvelope {
        status: 431,
        headers: BTreeMap::new(),
        body: TOO_MANY_HEADERS_MSG.to_owned(),
        metadata: ResponseMetadata::current(),
    }
}

/// Status for a wire-header parse failure: `431` when the header carried
/// more than [`crate::wire::MAX_REQUEST_HEADERS`] headers, else `400`.
/// Cold path only (runs on a parse error).
fn parse_error_status(msg: &str) -> u16 {
    if msg.ends_with(TOO_MANY_HEADERS_MSG) {
        431
    } else {
        400
    }
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
        header.connection_info(),
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

/// Preserves the derived `ResponseEnvelope` JSON shape if serialization fails.
fn envelope_serialization_fallback() -> String {
    String::from(ENVELOPE_SERIALIZATION_FALLBACK)
}

/// Dispatch a [`RequestEnvelope`] through an axum [`Router`] and
/// return the serialised [`ResponseEnvelope`] JSON.
///
/// This borrows the envelope and clones its owned fields before
/// passing them to the hot path.  Callers that already own a
/// [`RequestEnvelope`] should prefer [`dispatch_owned`] to skip the
/// clone.
pub async fn dispatch(router: Router, envelope: &RequestEnvelope) -> String {
    let result = dispatch_owned(router, envelope.clone()).await;
    // The fallback is unreachable in practice: `ResponseEnvelope` derives
    // `Serialize` over only primitives, `String`, `BTreeMap`, and
    // `Cow<'static, str>` — none of which can `Err` in `serde_json`.  Keeping
    // it (instead of unwrapping) leaves this public direct-API entry free of
    // unwind sites, matching the no-panic discipline documented in
    // [`crate::internal::router_oneshot`], [`crate::wire::header_write`], and
    // [`crate::wire::header_read::expect_literal`].
    serde_json::to_string(&result).unwrap_or_else(|_| envelope_serialization_fallback())
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
    // `http::HeaderMap` panics past 32 768 entries; the wire parser caps
    // the count at `MAX_REQUEST_HEADERS`, this envelope entry does it here
    // (kept out of the shared request builder so the buffered fast path's
    // codegen is unchanged).
    if headers.len() > MAX_REQUEST_HEADERS {
        return too_many_headers_envelope();
    }
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
///
/// When a [`crate::dispatch_timeout`] is configured, the whole dispatch
/// (handler + body drain) races it and an expired deadline yields a `504`
/// wire response.  A body larger than [`crate::max_buffered_response_bytes`]
/// yields a `500` wire response naming the limit.
pub async fn dispatch_from_bytes_async(input: Vec<u8>) -> Vec<u8> {
    traced!(crate::obs::buffered_mode(), {
        Deadline::start()
            .run(dispatch_from_bytes_inner(input))
            .await
            .unwrap_or_else(timeout_wire)
    })
}

async fn dispatch_from_bytes_inner(input: Vec<u8>) -> Vec<u8> {
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
/// `status == 422` first prefetches at most `MAX_HOIST_BODY_BYTES + 1` bytes
/// ([`prefetch_422_body`]): a small body is materialised via [`to_wire_bytes`]
/// so `validation_errors` hoisting is preserved byte-for-byte, while an
/// oversized 422 continues through the ordinary (non-hoisting) drain below —
/// a huge 422 body is never collected just to be skipped by the hoist.  A
/// body-stream error discards the partial buffer and returns
/// `error_wire(500, ...)` (never a truncated success), and a body larger than
/// [`max_buffered_response_bytes`] returns a `500` naming the limit.
async fn finish_buffered_wire(
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    mut body: Body,
) -> Vec<u8> {
    let max = max_buffered_response_bytes();
    if status == 422 {
        match prefetch_422_body(body).await {
            Prefetched422::Small(bytes) => {
                if max != 0 && bytes.len() > max {
                    return response_too_large_wire(max);
                }
                return to_wire_bytes((status, headers, bytes, metadata));
            }
            Prefetched422::Large(rest) => body = rest,
            Prefetched422::Failed => return error_wire(500, BODY_STREAM_ERROR_MSG),
        }
    }

    // Size the final buffer up front: 4-byte length prefix + adaptive header
    // estimate (floored at WIRE_HEADER_RESERVE so small-header responses
    // never reserve less than before) + the body's exact size when the body
    // reports one (Full bodies do), so a single-frame response serializes
    // with zero reallocations.
    let header_cap = header_capacity_with_floor(&headers, &metadata);
    let body_cap = usize::try_from(body.size_hint().exact().unwrap_or(0)).unwrap_or(usize::MAX);
    // A body that announces (exact size hint) more than the cap is rejected
    // before a single frame is drained.
    if max != 0 && body_cap > max {
        return response_too_large_wire(max);
    }
    // Saturating so a pathological exact body hint cannot wrap the capacity
    // arithmetic; the reservation is FALLIBLE so an unallocatable response
    // degrades to a `500` wire response instead of aborting the host process.
    let want = 4usize.saturating_add(header_cap).saturating_add(body_cap);
    let mut out = Vec::new();
    if out.try_reserve_exact(want).is_err() {
        return response_alloc_failed_wire(want);
    }
    // The `false` arm is unreachable for a real `HeaderMap` (4 GiB+ of header
    // JSON); it emits a 500 wire response rather than panicking on the
    // response path.
    if write_wire_header_into_vec(&mut out, status, &headers, &metadata) {
        drain_buffered_body(out, body, max).await
    } else {
        error_wire(500, HEADER_TOO_LARGE_MSG)
    }
}

/// Appends complete body frames — capped at `max` body bytes (`0` =
/// unlimited) and grown fallibly — while replacing any failed stream with a
/// 500 wire response.
async fn drain_buffered_body(mut out: Vec<u8>, mut body: Body, max: usize) -> Vec<u8> {
    let mut body_len = 0usize;
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref()
                    && !data.is_empty()
                    && let Err(wire) = append_capped(&mut out, data, &mut body_len, max)
                {
                    return wire;
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
/// * **`422` responses** up to the 64 KiB hoist bound are materialised
///   first so the `validation_errors` hoisting into the wire header (see
///   [`dispatch_from_bytes`]) is preserved byte-for-byte — validation
///   failures are tiny and cold, correctness wins.  A larger 422 body is
///   written directly, unhoisted.
/// * **Pre-dispatch errors** (malformed wire, bad version, unknown
///   app, invalid method) write the small `error_wire` response.
///
/// # Overflow semantics
///
/// If `out` is too small the **exact** required size is reported via
/// [`DirectWriteResult::Overflow`].  An exact-length body (a `Full`
/// response / explicit `Content-Length`) reports it immediately from the
/// body's size hint **without draining**; an unknown-length (streaming)
/// body is drained (counting, not writing) to compute the size — bounded by
/// [`crate::max_buffered_response_bytes`], past which a `500` wire response
/// naming the limit is written instead.  Either way the handler has already
/// run; retrying runs it again — callers must gate retries on idempotency,
/// or use [`dispatch_into_async_borrowed_resumable`], which keeps the
/// overflowing response instead.
///
/// A configured [`crate::dispatch_timeout`] that expires writes a `504`
/// wire response into `out` instead.
pub async fn dispatch_into_async(input: Vec<u8>, out: &mut [u8]) -> DirectWriteResult {
    traced!("direct", {
        Deadline::start()
            .run(dispatch_into_inner(input, &mut *out))
            .await
            .unwrap_or_else(|| write_wire_into(out, &timeout_wire()))
    })
}

async fn dispatch_into_inner(input: Vec<u8>, out: &mut [u8]) -> DirectWriteResult {
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
    traced!("direct", {
        Deadline::start()
            .run(dispatch_into_borrowed_inner(input, &mut *out))
            .await
            .unwrap_or_else(|| write_wire_into(out, &timeout_wire()))
    })
}

async fn dispatch_into_borrowed_inner(input: &[u8], out: &mut [u8]) -> DirectWriteResult {
    match dispatch_borrowed_to_parts(input).await {
        Ok((status, headers, metadata, body)) => {
            finish_direct_write(out, status, headers, metadata, body).await
        }
        Err(wire) => write_wire_into(out, &wire),
    }
}

/// **Resumable** sibling of [`dispatch_into_async_borrowed`]: identical
/// for a response that fits `out`, but a response that does not is **kept**
/// and returned as [`ResumableDirectWrite::Overflow`] instead of being
/// discarded — continue it with
/// [`PendingDirectResponse::write_into`](crate::PendingDirectResponse::write_into)
/// (a larger buffer) or
/// [`PendingDirectResponse::stream`](crate::PendingDirectResponse::stream)
/// without re-running the handler.
///
/// Nothing past `out` is drained: the response body stops being read the
/// moment a frame does not fit (or up front, when its exact size hint
/// already exceeds `out`), so an unbounded body never blocks or buffers
/// here.  A `text/event-stream` response is returned pending immediately
/// ([`PendingDirectResponse::prefers_streaming`](crate::PendingDirectResponse::prefers_streaming))
/// because it may never end.  A configured [`crate::dispatch_timeout`]
/// bounds the call (expiry → `504` wire, written into `out`).
pub async fn dispatch_into_async_borrowed_resumable(
    input: &[u8],
    out: &mut [u8],
) -> ResumableDirectWrite {
    traced!("direct", {
        let done = Deadline::start()
            .run(dispatch_into_borrowed_resumable_inner(input, &mut *out))
            .await;
        done.unwrap_or_else(|| resumable_from_wire(out, timeout_wire()))
    })
}

async fn dispatch_into_borrowed_resumable_inner(
    input: &[u8],
    out: &mut [u8],
) -> ResumableDirectWrite {
    match dispatch_borrowed_to_parts(input).await {
        Ok((status, headers, metadata, body)) => {
            finish_resumable(out, status, headers, metadata, body, true).await
        }
        Err(wire) => resumable_from_wire(out, wire),
    }
}

/// Shared prelude of the borrowed direct-write dispatchers: ingress cap →
/// borrowed split → parse/validate/resolve → dispatch.  `Err` carries the
/// ready-to-deliver wire error.
async fn dispatch_borrowed_to_parts(
    input: &[u8],
) -> Result<(u16, http::HeaderMap, ResponseMetadata, Body), Vec<u8>> {
    // Ingress cap (defense-in-depth) — same policy as `dispatch_into_async`.
    if let Some(err) = check_ingress_cap(input.len()) {
        return Err(err);
    }
    let (header_bytes, body_bytes) =
        split_wire_borrowed(input).map_err(|msg| error_wire(400, &msg))?;
    let (header, router) = parse_validate_resolve(header_bytes)?;

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
    dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        None,
        header.iter_str_pairs(),
        None,
        body,
        default_json_when_absent,
        header.connection_info(),
    )
    .await
    .map_err(|(status, msg)| error_wire(status, &msg))
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

        let fallback = envelope_serialization_fallback();
        assert_eq!(derived, fallback);

        let parsed: serde_json::Value =
            serde_json::from_str(&fallback).expect("fallback must remain valid JSON");
        assert_eq!(parsed["status"].as_u64(), Some(500));
        assert_eq!(parsed["headers"], serde_json::json!({}));
        assert_eq!(
            parsed["body"].as_str(),
            Some("envelope serialization failed")
        );
        assert_eq!(
            parsed["metadata"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION"))
        );
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
