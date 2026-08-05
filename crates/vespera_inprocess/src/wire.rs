//! Binary wire format: request-header borrowing deserialization,
//! response-header serialization (straight from `http::HeaderMap`),
//! frame split/parse, and 422 `validation_errors` hoisting.
//!
//! The serialized byte layout is **locked** by tests/wire_contract.rs.

use std::borrow::Cow;

use bytes::Bytes;

use crate::envelope::ResponseMetadata;
use crate::internal::{HEADER_TOO_LARGE_MSG, ResponseParts};

/// Hand-rolled request-header parser (byte-compatible replacement for
/// the `serde_json` derive path; the serde version is retained as
/// `bench_serde::parse_wire_header_serde` for the criterion A/B).
mod header_read;
/// Hand-rolled response-header serializer (byte-identical to the
/// `serde_json` path retained as
/// `bench_serde::write_wire_header_into_slice_serde` for the criterion A/B).
mod header_write;
pub mod hoist;

/// Bench-only `serde_json` twins of the hand-rolled parser/serializer.
/// Compiled only under `test` / `bench-support`; production wire code
/// never reaches this module.
#[cfg(any(test, feature = "bench-support"))]
mod bench_serde;

/// Criterion A/B surface re-exported from [`bench_serde`] so
/// `crate::bench_support` in [`crate::lib`] finds these under
/// `crate::wire::bench_*` (unchanged public path).
#[cfg(any(test, feature = "bench-support"))]
pub use bench_serde::{bench_parse_hand, bench_parse_serde, bench_write_hand, bench_write_serde};
/// Serde-driven deserializer helpers used by the `#[cfg_attr(...,
/// derive(serde::Deserialize))]` on [`WireRequestHeader`] — they live in
/// [`bench_serde`] alongside the rest of the bench-only serde twin.
#[cfg(any(test, feature = "bench-support"))]
use bench_serde::{de_cow_pairs, de_opt_cow};

use header_write::JsonSink;

#[cfg(test)]
mod tests;

/// Wire format protocol version.  The JSON header's `v` field MUST
/// equal this for requests; responses always emit this value.
pub const WIRE_VERSION: u8 = 1;

// ── Wire Format Types (internal) ─────────────────────────────────────

/// Request wire header.  In production it is built **by the hand-rolled
/// [`header_read`] parser**, which borrows every plain string straight from
/// the wire bytes (zero allocation) and owns only escaped strings.
///
/// The `serde` `Deserialize` derive (plus the `BorrowableCow` /
/// `de_cow_pairs` / `de_opt_cow` helpers in [`bench_serde`]) is compiled
/// **only under the `bench-support` feature**, where
/// `bench_serde::parse_wire_header_serde` uses it as the criterion A/B
/// "before" arm — the production path never goes through serde, so it is
/// not part of the shipped build.
#[derive(Debug)]
#[cfg_attr(any(test, feature = "bench-support"), derive(serde::Deserialize))]
pub struct WireRequestHeader<'a> {
    /// Wire protocol version; clients MUST send 1.
    #[cfg_attr(any(test, feature = "bench-support"), serde(default))]
    pub v: u8,
    #[cfg_attr(any(test, feature = "bench-support"), serde(borrow))]
    pub method: Cow<'a, str>,
    #[cfg_attr(any(test, feature = "bench-support"), serde(borrow))]
    pub path: Cow<'a, str>,
    #[cfg_attr(any(test, feature = "bench-support"), serde(default, borrow))]
    pub query: Cow<'a, str>,
    /// Request headers as a flat list — dispatch only ever *iterates*
    /// them (never looks one up by key), so a `Vec` skips the
    /// `HashMap` bucket allocation + per-key hashing entirely.
    /// Repeated names are forwarded as repeated request headers
    /// (valid HTTP; the previous `HashMap` silently kept the last
    /// duplicate of a degenerate duplicate-key JSON header).
    #[cfg_attr(
        any(test, feature = "bench-support"),
        serde(default, borrow, deserialize_with = "de_cow_pairs")
    )]
    pub headers: CowPairs<'a>,
    /// Optional name of the target app for multi-app routing.  When
    /// omitted (or empty), the request is dispatched to the default
    /// app registered via [`register_app`].  Use [`register_app_named`]
    /// to register additional named apps.
    #[cfg_attr(
        any(test, feature = "bench-support"),
        serde(default, borrow, deserialize_with = "de_opt_cow")
    )]
    pub app: Option<Cow<'a, str>>,
}

impl WireRequestHeader<'_> {
    /// Iterate the parsed request headers as `(&str, &str)` pairs — the
    /// only shape every wire dispatch entry point hands to
    /// `dispatch_and_split` / `dispatch_response_streaming` /
    /// `dispatch_parts`.  Centralised so every call site stays a single
    /// `header.iter_str_pairs()` call instead of duplicating the same
    /// `map(|(k, v)| (k.as_ref(), v.as_ref()))` closure across six
    /// dispatch / streaming sites.  `#[inline]` keeps the generated code
    /// byte-identical to the prior inlined closures.
    #[inline]
    pub fn iter_str_pairs(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref()))
    }
}

/// Flat list of `(name, value)` request-header pairs borrowing from
/// the wire input.  Also referenced from [`bench_serde`] via
/// `super::CowPairs` (private-item visibility to descendant modules) —
/// keeps a single canonical shape for both the production hand-rolled
/// parser and the bench-only serde twin.
type CowPairs<'a> = Vec<(Cow<'a, str>, Cow<'a, str>)>;

/// Minimum byte capacity reserved for a serialized response wire header
/// (the `[u32 BE header_len | header JSON]` prefix).  This is the *floor*
/// every buffered wire-header sizing site applies, via
/// [`header_capacity_with_floor`], on top of the adaptive
/// [`header_capacity_estimate`].
///
/// Typical wire headers are well under this reservation, so the
/// serializer usually writes without reallocating.
pub const WIRE_HEADER_RESERVE: usize = 192;

/// Header-name sort stack-buffer capacity — shared by the production
/// [`header_write::write_headers`] serializer and the bench-only serde
/// twin `bench_serde::WireHeaders::serialize`.  Both arms of the same-run
/// `wire_header_serde` criterion A/B must size their stack buffer
/// identically (the bench's invariant is "same code, different
/// serializer" — diverging caps would silently compare unequal
/// stack/heap fallback behaviour).  Declaring this once here makes the
/// invariant impossible to break by editing only one site.
const STACK_CAP: usize = 32;

/// Cheap upper-ish estimate of the serialized response wire-header JSON
/// byte length (excluding the 4-byte length prefix), so the response
/// `Vec` can be sized to serialize a header-heavy response **without
/// reallocating**.  Counts the fixed JSON scaffolding + version string +
/// each header's `"name":"value",` rendering (a repeated name is counted
/// once per value — a safe over-estimate).  Escape-heavy values can still
/// exceed it (rare → one growth); this only sets capacity, never the
/// emitted bytes.  Always combined with a [`WIRE_HEADER_RESERVE`] floor by
/// callers, so a small-header response never reserves less than before.
pub fn header_capacity_estimate(headers: &http::HeaderMap, metadata: &ResponseMetadata) -> usize {
    // `{"v":1,"status":NNN,"headers":{},"metadata":{"version":""}}` scaffold —
    // self-documenting via `str::len()` of the literal so a future scaffold
    // edit (new metadata field, renamed key, …) auto-recomputes the floor
    // instead of silently desynchronising a hand-counted constant.  The 3
    // `NNN` status bytes are baked into the literal: `http::StatusCode` is
    // constrained to 100..=999, so the serialized status is *always* exactly
    // 3 digits.  `str::len()` has been `const` since Rust 1.39, so this
    // still evaluates at compile time (== 59 bytes, byte-identical capacity).
    const SCAFFOLD: usize =
        "{\"v\":1,\"status\":NNN,\"headers\":{},\"metadata\":{\"version\":\"\"}}".len();
    let mut est = SCAFFOLD + metadata.version.len();
    for (name, value) in headers {
        est += name.as_str().len() + value.len() + 8;
    }
    est
}

/// Adaptive response wire-header capacity estimate, floored at
/// [`WIRE_HEADER_RESERVE`] so small-header responses never reserve less
/// than before. Locks the floor invariant in one place — every buffered
/// wire-header sizing site goes through this helper, so a future sizing
/// site cannot accidentally forget the `.max(WIRE_HEADER_RESERVE)` call.
///
/// `#[inline]` so the codegen at each call site matches the prior inlined
/// `header_capacity_estimate(...).max(WIRE_HEADER_RESERVE)` expression
/// byte-for-byte.
#[inline]
pub fn header_capacity_with_floor(headers: &http::HeaderMap, metadata: &ResponseMetadata) -> usize {
    header_capacity_estimate(headers, metadata).max(WIRE_HEADER_RESERVE)
}

/// Cheap upper-ish estimate of the serialized `validation_errors` JSON
/// array byte length, added to the response-`Vec` capacity **only on the
/// 422 path** (`validation_errors` is `None` for every other status, so the
/// hot success path pays nothing).  Each item renders as
/// `{"path":"…","code":"…","message":"…"},` inside the
/// `,"validation_errors":[…]` wrapper — count the field bytes plus a fixed
/// per-item scaffold.  A safe over-estimate (absent `code`/`message` only
/// shrink the real output), so it only ever prevents the mid-serialize
/// realloc the hoisted errors would otherwise force; it never changes the
/// emitted bytes.
fn validation_errors_capacity_estimate(items: &[ValidationErrorItem]) -> usize {
    // `,"validation_errors":[]` wrapper, plus per item the
    // `{"path":"","code":"","message":""},` scaffold — both sized via
    // `str::len()` of the literals they document so a future shape change
    // (renamed field, extra field) auto-recomputes the floor instead of
    // silently desynchronising a hand-counted constant.  The literals'
    // empty values are themselves the worst case: absent `code`/`message`
    // only shrink the rendered output, so this stays a safe upper bound.
    const WRAPPER: usize = ",\"validation_errors\":[]".len();
    const ITEM_SCAFFOLD: usize = "{\"path\":\"\",\"code\":\"\",\"message\":\"\"},".len();
    let mut est = WRAPPER;
    for item in items {
        est += ITEM_SCAFFOLD
            + item.path.len()
            + item.code.as_deref().map_or(0, str::len)
            + item.message.as_deref().map_or(0, str::len);
    }
    est
}

/// Append `[u32 BE header_len | header JSON]` to `out`.  Returns `false`
/// when the serialized header JSON exceeds `u32::MAX` bytes — unreachable for
/// any real `HeaderMap` (4 GiB of header JSON), so callers map it to a `500`
/// wire response instead of panicking on the response path.
#[must_use]
fn write_wire_header_into(
    out: &mut Vec<u8>,
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
    validation_errors: Option<&[ValidationErrorItem]>,
) -> bool {
    out.extend_from_slice(&[0u8; 4]);
    let start = out.len();
    header_write::write_response_header(out, status, headers, metadata, validation_errors);
    // A serialized response header never approaches `u32::MAX` (4 GiB of
    // header JSON is unreachable for any real `HeaderMap`); on the impossible
    // overflow report `false` so the caller emits a `500` rather than
    // panicking on the response path.
    let Ok(header_len) = u32::try_from(out.len() - start) else {
        return false;
    };
    out[start - 4..start].copy_from_slice(&header_len.to_be_bytes());
    true
}

/// Allocate a response `Vec` sized for `[u32 BE header_len | header JSON]`
/// (plus `body_reserve` bytes of body the caller appends afterwards) and
/// serialize the wire header into it.  `None` signals the unreachable `u32`
/// header-length overflow, which every caller maps to
/// `error_wire(500, HEADER_TOO_LARGE_MSG)`.
///
/// Single home for the sizing rule [`to_wire_bytes`],
/// [`build_wire_header_bytes`] and [`build_wire_header_bytes_hoisting`]
/// each used to spell out verbatim: [`header_capacity_with_floor`] plus,
/// on the 422 path only, [`validation_errors_capacity_estimate`].
/// `#[inline]` keeps each call site's codegen identical to the block it
/// replaced (`body_reserve` is a constant `0` at two of the three), so
/// reserved capacity and emitted bytes are unchanged.
#[inline]
fn build_header_vec(
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
    validation_errors: Option<&[ValidationErrorItem]>,
    body_reserve: usize,
) -> Option<Vec<u8>> {
    let header_cap = header_capacity_with_floor(headers, metadata)
        + validation_errors.map_or(0, validation_errors_capacity_estimate);
    // `4 + header_cap + body_reserve` cannot overflow `usize` on a 64-bit
    // target (it would require a multi-exabyte body); plain `+` is used so
    // the hot response path keeps its exact arithmetic — a `saturating_add`
    // variant was benchmarked and cost ~2-3% on the small
    // `wire_path`/`request_headers_path` cases for zero real-world benefit.
    // The `validation_errors` term is `0` for every non-422 response (the hot
    // success path is byte-for-byte unchanged); on the 422 path it sizes the
    // `Vec` to serialise the hoisted errors without the mid-write realloc a
    // hoist-blind estimate paid (locked by tests/alloc_budget.rs case F).
    let mut out = Vec::with_capacity(4 + header_cap + body_reserve);
    if write_wire_header_into(&mut out, status, headers, metadata, validation_errors) {
        Some(out)
    } else {
        None
    }
}

/// Append `[u32 BE header_len | header JSON]` (no `validation_errors`)
/// straight into `out` — the `Vec`-appending sibling of
/// [`write_wire_header_into_slice`], used by the buffered direct-streaming
/// response assembler (`dispatch::finish_buffered_wire`).  Wraps the
/// private [`write_wire_header_into`] so the internal [`ValidationErrorItem`]
/// type stays out of the crate-visible surface.
#[must_use]
pub fn write_wire_header_into_vec(
    out: &mut Vec<u8>,
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> bool {
    write_wire_header_into(out, status, headers, metadata, None)
}

/// One entry in the wire header's `validation_errors` array.  Fields
/// are best-effort: missing values in the source body become `None`.
/// The `Serialize` derive is **bench-only** — production serializes these
/// fields with the hand-rolled `header_write` writer, never via serde.
#[derive(Debug)]
#[cfg_attr(any(test, feature = "bench-support"), derive(serde::Serialize))]
struct ValidationErrorItem {
    path: String,
    #[cfg_attr(
        any(test, feature = "bench-support"),
        serde(skip_serializing_if = "Option::is_none")
    )]
    code: Option<String>,
    #[cfg_attr(
        any(test, feature = "bench-support"),
        serde(skip_serializing_if = "Option::is_none")
    )]
    message: Option<String>,
}

/// Build a wire-format error response with a plain-text body.
///
/// Used by [`dispatch_from_bytes`] for malformed input and by the
/// JNI bridge for panic fallback.  The response always carries
/// `content-type: text/plain; charset=utf-8`.
#[must_use]
pub fn error_wire(status: u16, msg: &str) -> Vec<u8> {
    let mut headers = http::HeaderMap::with_capacity(1);
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    let metadata = ResponseMetadata::current();
    // Write the header + plain-text body straight into one buffer.  An error
    // body is never JSON, so it never participates in 422 `validation_errors`
    // hoisting — routing through `to_wire_bytes` would only add an
    // intermediate `Bytes::copy_from_slice(msg)` allocation plus a second copy
    // of the same bytes into the final `Vec`.  The error header is a single
    // `content-type`, so it can never approach `u32::MAX`; the
    // `write_wire_header_into` overflow signal is unreachable here and ignored.
    let body = msg.as_bytes();
    // The fixed single `content-type: text/plain; charset=utf-8` header plus
    // the SemVer version field stays well under `WIRE_HEADER_RESERVE` (192
    // bytes): `SCAFFOLD(59) + version.len() + (12 + 26 + 8) = 105 +
    // version.len()`.  Even a pathological 80-byte SemVer still fits, so the
    // `.max(WIRE_HEADER_RESERVE)` floor of the prior
    // `header_capacity_estimate(...).max(WIRE_HEADER_RESERVE)` call ALWAYS
    // won — the estimate call and its `HeaderMap` iteration were pure wasted
    // work on every error response (called from every malformed-wire /
    // wrong-version / unknown-app / panic-fallback path).  The debug-assert
    // below locks the invariant: if a future change (a longer SemVer, a
    // second baked-in header) ever pushes the estimate over the floor, debug
    // builds fail loudly so the floor can be revisited.
    debug_assert!(
        header_capacity_estimate(&headers, &metadata) <= WIRE_HEADER_RESERVE,
        "error_wire header estimate must fit WIRE_HEADER_RESERVE"
    );
    let header_cap = WIRE_HEADER_RESERVE;
    let mut out = Vec::with_capacity(4 + header_cap + body.len());
    let _ = write_wire_header_into(&mut out, status, &headers, &metadata, None);
    out.extend_from_slice(body);
    out
}

/// Adapter: response parts → wire-format bytes.  Layout:
/// `[u32 BE header_len | JSON header | raw body]`.
///
/// **Reached only from [`crate::dispatch::build_422_wire`]** — a `422`
/// response must materialise the whole wire tail into an intermediate
/// `Vec` so its JSON body can be inspected and, on a best-effort basis,
/// any `{"errors": [...]}` payload hoisted into the wire header's
/// `validation_errors` field.  Java decoders can then read validation
/// failures with a single header parse while the original body is
/// preserved verbatim for clients that still rely on it.
///
/// Every non-422 status **bypasses this adapter entirely**: the buffered
/// and direct-write dispatchers stream response frames straight into the
/// destination buffer via [`crate::dispatch::finish_buffered_wire`] and
/// [`crate::dispatch::finish_direct_write`], avoiding the intermediate
/// `Vec` copy this helper is only worth on the cold 422 path.  Kept `pub`
/// (not `pub(crate)`) because `alloc_budget.rs` case F asserts the 422
/// materialise path allocates a known-bounded number of buffers and its
/// docstring anchors to this symbol.
pub fn to_wire_bytes(parts: ResponseParts) -> Vec<u8> {
    let (status, headers, body_bytes, metadata) = parts;
    let validation_errors = if status == 422 {
        hoist::try_hoist_validation_errors(&headers, &body_bytes)
    } else {
        None
    };
    let Some(mut out) = build_header_vec(
        status,
        &headers,
        &metadata,
        validation_errors.as_deref(),
        body_bytes.len(),
    ) else {
        // Unreachable for a real `HeaderMap` (would need 4 GiB+ of header
        // JSON); never panic on the response path — emit a 500 instead.
        return error_wire(500, HEADER_TOO_LARGE_MSG);
    };
    out.extend_from_slice(&body_bytes);
    out
}

/// Build wire-format header bytes (`[u32 BE header_len | JSON header]`)
/// without a body — used by the `*_with_header` callback variants.
///
/// Sizes the buffer with the adaptive [`header_capacity_estimate`] (floored
/// at [`WIRE_HEADER_RESERVE`] so small-header responses never reserve less
/// than before), matching [`to_wire_bytes`] / `finish_buffered_wire`: a
/// many-header streaming response now serializes its header without the
/// mid-write reallocation the flat `WIRE_HEADER_RESERVE` reserve forced.
pub fn build_wire_header_bytes(
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> Vec<u8> {
    let Some(out) = build_header_vec(status, headers, metadata, None, 0) else {
        // Unreachable for a real `HeaderMap`; never panic on the response path.
        return error_wire(500, HEADER_TOO_LARGE_MSG);
    };
    out
}

/// Build wire-format header bytes (`[u32 BE header_len | JSON header]`) for the
/// header-first streaming paths, **hoisting 422 `validation_errors`** from
/// `body` into the header — the same contract the buffered [`to_wire_bytes`]
/// upholds.  Java/FFI decoders can then read validation failures from the wire
/// header in EVERY dispatch mode (not just buffered / direct); the caller still
/// delivers the original body verbatim through its chunk sink.
///
/// For any non-422 status `body` is ignored and the output is byte-identical to
/// [`build_wire_header_bytes`] (the hot success path pays nothing).
pub fn build_wire_header_bytes_hoisting(
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
    body: &Bytes,
) -> Vec<u8> {
    if status != 422 {
        return build_wire_header_bytes(status, headers, metadata);
    }
    let validation_errors = hoist::try_hoist_validation_errors(headers, body);
    let Some(out) = build_header_vec(status, headers, metadata, validation_errors.as_deref(), 0)
    else {
        // Unreachable for a real `HeaderMap`; never panic on the response path.
        return error_wire(500, HEADER_TOO_LARGE_MSG);
    };
    out
}

/// Write `[u32 BE header_len | JSON header]` **straight into `out`**
/// with the hand-rolled [`header_write`] serializer, returning the exact
/// total header byte count regardless of whether it fit.  The
/// direct-write sibling of [`build_wire_header_bytes`] — no intermediate
/// `Vec`, byte-identical output to the previous `serde_json` path
/// (retained as `bench_serde::write_wire_header_into_slice_serde` for
/// the criterion A/B).
///
/// When the header fits (`returned <= out.len()`) `out[0..returned]`
/// holds the complete header.  When it does not fit, `out`'s contents are
/// partial/undefined (per the direct-write `Overflow` contract) but the
/// returned count is still exact, so the caller can report the precise
/// required size.
pub fn write_wire_header_into_slice(
    out: &mut [u8],
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> usize {
    let header_total = {
        let mut sink = header_write::SliceSink::new(out);
        // Reserve the 4-byte length prefix, then serialize the JSON body
        // straight after it; backfilled below once the length is known.
        sink.put(&[0u8; 4]);
        header_write::write_response_header(&mut sink, status, headers, metadata, None);
        sink.pos
    };
    if header_total <= out.len()
        && let Ok(json_len) = u32::try_from(header_total - 4)
    {
        // `json_len` only overflows `u32` when the header JSON exceeds 4 GiB,
        // which requires `out` itself to exceed 4 GiB — unreachable for any
        // real buffer.  Leave the length prefix zeroed in that impossible
        // case rather than panicking; the exact `header_total` is still
        // returned so the caller reports the precise required size.
        out[0..4].copy_from_slice(&json_len.to_be_bytes());
    }
    header_total
}

/// Hard upper bound on the wire header-JSON region, enforced **before**
/// any parse or allocation work.  The header carries method/path/query
/// plus the request headers as JSON; a legitimate header set is at most a
/// few tens of KiB, so 1 MiB is generous headroom while bounding the parse
/// work + header-vector allocation an attacker-controlled `header_len` can
/// force on a direct FFI caller (the Spring proxy is already
/// servlet-header-capped upstream).  An oversized header is rejected with a
/// wire `400` rather than parsed.
const MAX_WIRE_HEADER_BYTES: usize = 1024 * 1024;

/// Reject a decoded `header_len` that exceeds [`MAX_WIRE_HEADER_BYTES`]
/// before the header region is sliced or parsed.
fn check_header_len(header_len: usize) -> Result<(), String> {
    if header_len > MAX_WIRE_HEADER_BYTES {
        return Err(format!(
            "wire header_len ({header_len}) exceeds maximum of {MAX_WIRE_HEADER_BYTES} bytes"
        ));
    }
    Ok(())
}

/// Validate the wire frame prefix and return the absolute byte offset at
/// which the request body region begins (= `4 + header_len`).  Shared by
/// [`split_wire_request`] (owned) and [`split_wire_borrowed`] (borrowed) —
/// the two used to inline the same prefix/length/range validation;
/// centralising it removes the duplicate logic and guarantees the two
/// paths can never drift in their error messages (the existing
/// [`crate::tests::wire_contract`] and [`crate::tests::wire_robustness`]
/// behavioural goldens stay byte-identical because the validation bytes
/// are the same).
///
/// `#[inline]` so both callers inline the helper exactly as before the
/// extraction — the helper has one branch per error site, so the inlined
/// codegen at the call site matches the pre-refactor codegen.
#[inline]
fn parse_wire_header_len(input: &[u8]) -> Result<usize, String> {
    if input.len() < 4 {
        return Err(format!(
            "wire input too short: {} bytes, need at least 4",
            input.len()
        ));
    }
    // `try_into()` cannot fail: the `input.len() < 4` guard above proves the
    // slice is exactly four bytes long.  The `.expect` is structural
    // documentation of that invariant, never reached at runtime.  Byte-identical
    // codegen to the prior `let mut + copy_from_slice` triplet (LLVM lowers both
    // to the same `bswap`-style instruction) — the wire-contract goldens and
    // the hand-vs-serde round-trip property test lock the byte behaviour.
    let header_len =
        u32::from_be_bytes(input[..4].try_into().expect("bounds checked above")) as usize;
    check_header_len(header_len)?;
    let total_header_end = 4usize.saturating_add(header_len);
    if total_header_end > input.len() {
        return Err(format!(
            "wire header_len ({header_len}) exceeds remaining input ({} bytes)",
            input.len() - 4
        ));
    }
    Ok(total_header_end)
}

/// Split a wire-format request into its header-JSON region and body —
/// both true zero-copy O(1) refcount views of the input allocation
/// (unlike `Vec::split_off`, which allocates a new vector and memcpys
/// the tail).
///
/// Two-phase with [`parse_wire_header`] so the deserialized header
/// can **borrow** its strings from the returned header bytes (the
/// caller keeps them alive on its stack frame).
pub fn split_wire_request(input: Vec<u8>) -> Result<(Bytes, Bytes), String> {
    let total_header_end = parse_wire_header_len(&input)?;
    // `Bytes::from(Vec<u8>)` is O(1) (Arc wrap, no copy), so the order of
    // validation vs Bytes wrap has no runtime effect.
    let mut input = Bytes::from(input);
    // O(1) splits: all views share the original allocation.
    let body = input.split_off(total_header_end);
    let header_json = input.slice(4..);
    Ok((header_json, body))
}

/// Borrowing sibling of [`split_wire_request`]: returns the header-JSON
/// region and body region as **sub-slices of `input`** — zero allocation,
/// zero refcount (unlike [`split_wire_request`], which wraps the input in
/// a `Bytes`).  The caller MUST keep `input` alive for as long as the
/// returned slices — and anything borrowing from them — are used.
pub fn split_wire_borrowed(input: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let total_header_end = parse_wire_header_len(input)?;
    Ok((&input[4..total_header_end], &input[total_header_end..]))
}

/// Deserialize the wire request header, borrowing every string from
/// `header_json` where possible (see [`WireRequestHeader`]).
///
/// Uses the hand-rolled [`header_read`] parser — byte-behaviour-identical
/// to the previous `serde_json` derive path (retained as
/// `bench_serde::parse_wire_header_serde` for the criterion A/B): any key
/// order, unknown keys ignored, plain strings borrowed / escaped strings
/// owned.
#[inline]
pub fn parse_wire_header(header_json: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    header_read::parse(header_json).map_err(|e| format!("wire header JSON parse error: {e}"))
}
