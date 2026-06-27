//! Binary wire format: request-header borrowing deserialization,
//! response-header serialization (straight from `http::HeaderMap`),
//! frame split/parse, and 422 `validation_errors` hoisting.
//!
//! The serialized byte layout is **locked** by tests/wire_contract.rs.

use std::borrow::Cow;

use bytes::Bytes;
// `Serialize` is used only by the bench-only serde wire-header twins.
#[cfg(any(test, feature = "bench-support"))]
use serde::Serialize;

use crate::envelope::ResponseMetadata;
use crate::internal::ResponseParts;

/// Hand-rolled request-header parser (byte-compatible replacement for
/// the `serde_json` derive path; the serde version is retained as
/// [`parse_wire_header_serde`] for the criterion A/B).
mod header_read;
/// Hand-rolled response-header serializer (byte-identical to the
/// `serde_json` path retained as [`write_wire_header_into_slice_serde`]
/// for the criterion A/B).
mod header_write;
pub mod hoist;

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
/// The `serde` `Deserialize` derive (plus the [`BorrowableCow`] /
/// `de_cow_pairs` / `de_opt_cow` helpers) is compiled **only under the
/// `bench-support` feature**, where [`parse_wire_header_serde`] uses it as
/// the criterion A/B "before" arm — the production path never goes through
/// serde, so it is not part of the shipped build.
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

/// `Cow<str>` wrapper whose `Deserialize` impl borrows from the input
/// when the JSON string carries no escape sequences.  Bench-only — feeds
/// the `serde` A/B twin; production parsing is hand-rolled ([`header_read`]).
#[cfg(any(test, feature = "bench-support"))]
struct BorrowableCow<'a>(Cow<'a, str>);

#[cfg(any(test, feature = "bench-support"))]
impl<'de> serde::Deserialize<'de> for BorrowableCow<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = BorrowableCow<'de>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string")
            }

            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                v: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(BorrowableCow(Cow::Borrowed(v)))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(BorrowableCow(Cow::Owned(v.to_owned())))
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(BorrowableCow(Cow::Owned(v)))
            }
        }
        deserializer.deserialize_str(V)
    }
}

/// Flat list of `(name, value)` request-header pairs borrowing from
/// the wire input.
type CowPairs<'a> = Vec<(Cow<'a, str>, Cow<'a, str>)>;

/// Deserialize a JSON object into a flat `Vec` of `(name, value)`
/// pairs whose strings borrow from the input where possible — one
/// `Vec` allocation instead of `HashMap` buckets + per-key hashing.
/// Bench-only (feeds the serde A/B twin).
#[cfg(any(test, feature = "bench-support"))]
fn de_cow_pairs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<CowPairs<'de>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = CowPairs<'de>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of strings")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((k, v)) =
                access.next_entry::<BorrowableCow<'de>, BorrowableCow<'de>>()?
            {
                out.push((k.0, v.0));
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(V)
}

/// Deserialize an `Option<Cow>` that borrows from the input where
/// possible.  Bench-only (feeds the serde A/B twin).
#[cfg(any(test, feature = "bench-support"))]
fn de_opt_cow<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Cow<'de, str>>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<Cow<'de, str>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or null")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: serde::Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            <BorrowableCow as serde::Deserialize>::deserialize(deserializer).map(|c| Some(c.0))
        }
    }
    deserializer.deserialize_option(V)
}

// wire-order locked — field order defines the serialized wire header
// byte layout (`v`, `status`, `headers`, `metadata`,
// `validation_errors?`).  See tests/wire_contract.rs.
#[cfg(any(test, feature = "bench-support"))]
#[derive(Debug, Serialize)]
struct WireResponseHeader<'a, H: Serialize> {
    v: u8,
    status: u16,
    headers: &'a H,
    metadata: &'a ResponseMetadata,
    /// Validation errors hoisted from a 422 JSON body so Java decoders
    /// can read them with a single header parse.  `None` for any other
    /// status; the original body is preserved verbatim regardless.
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_errors: Option<Vec<ValidationErrorItem>>,
}

/// Zero-allocation serializer for response headers: renders an
/// [`http::HeaderMap`] as the wire's sorted name → value JSON map,
/// borrowing every name and value straight from the map.
///
/// Byte-compatible with the previous `BTreeMap<String, HeaderValue>`
/// representation (locked by tests/wire_contract.rs):
/// - names sort in byte order (`HeaderName`s are lowercase ASCII, so
///   `sort_unstable` equals `BTreeMap` ordering)
/// - single-valued headers render as a JSON string, repeated names as
///   a JSON array in insertion order (the untagged `HeaderValue`
///   shape)
/// - non-UTF-8 header values render as `""` (same `unwrap_or("")`
///   behaviour as the old owned conversion)
#[cfg(any(test, feature = "bench-support"))]
struct WireHeaders<'a>(&'a http::HeaderMap);

#[cfg(any(test, feature = "bench-support"))]
impl Serialize for WireHeaders<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // `HeaderMap::keys` yields each distinct name exactly once.  The
        // overwhelmingly common response carries only a handful of header
        // names, so sort them in a stack buffer and skip the per-response
        // heap `Vec`; header sets larger than the stack cap fall back to a
        // heap `Vec`.  Output is byte-identical either way (same sorted
        // order over the same names), as locked by tests/wire_contract.rs.
        const STACK_CAP: usize = 32;
        let key_count = self.0.keys_len();
        let mut stack_names: [&str; STACK_CAP] = [""; STACK_CAP];
        let mut heap_names: Vec<&str>;
        let names: &mut [&str] = if key_count <= STACK_CAP {
            for (slot, name) in stack_names.iter_mut().zip(self.0.keys()) {
                *slot = name.as_str();
            }
            &mut stack_names[..key_count]
        } else {
            heap_names = Vec::with_capacity(key_count);
            heap_names.extend(self.0.keys().map(http::HeaderName::as_str));
            &mut heap_names[..]
        };
        names.sort_unstable();
        let mut map = serializer.serialize_map(Some(names.len()))?;
        for &name in names.iter() {
            let mut values = self.0.get_all(name).iter();
            let first = values
                .next()
                .expect("HeaderMap::keys yields only present names");
            if values.next().is_none() {
                map.serialize_entry(name, first.to_str().unwrap_or(""))?;
            } else {
                map.serialize_entry(name, &WireHeaderValues(self.0, name))?;
            }
        }
        map.end()
    }
}

/// Serializes the repeated values of one header name as a JSON array.
#[cfg(any(test, feature = "bench-support"))]
struct WireHeaderValues<'a>(&'a http::HeaderMap, &'a str);

#[cfg(any(test, feature = "bench-support"))]
impl Serialize for WireHeaderValues<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .get_all(self.1)
                .iter()
                .map(|v| v.to_str().unwrap_or("")),
        )
    }
}

/// Append `[u32 BE header_len | header JSON]` to `out`, serializing
/// the header **directly into the output buffer** with the hand-rolled
/// [`header_write`] serializer — no intermediate `Vec` and no second
/// memcpy of the header JSON.  Byte-identical to the previous
/// `serde_json::to_writer(WireResponseHeader { .. })` path (locked by
/// tests/wire_contract.rs).
///
/// Typical wire headers are well under this reservation, so the
/// serializer usually writes without reallocating.
pub const WIRE_HEADER_RESERVE: usize = 192;

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
    // {"v":1,"status":NNN,"headers":{},"metadata":{"version":""}} scaffold.
    // The 3 `NNN` status bytes are part of the fixed scaffold: `http::StatusCode`
    // is constrained to 100..=999, so the serialized status is *always* exactly
    // 3 digits. The prior value of 56 omitted them, under-counting every
    // estimate by 3 bytes (one needless Vec growth on header-heavy responses
    // whose estimate beat the WIRE_HEADER_RESERVE floor).
    const SCAFFOLD: usize = 59;
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
    // `{"path":"","code":"","message":""},` scaffold.
    const WRAPPER: usize = 24;
    const ITEM_SCAFFOLD: usize = 36;
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
#[cfg_attr(any(test, feature = "bench-support"), derive(Serialize))]
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
/// For `status == 422` JSON responses we **best-effort** hoist any
/// `{"errors": [...]}` payload into the wire header's
/// `validation_errors` field — Java decoders can read validation
/// failures with a single header parse, while the original body is
/// preserved verbatim for clients that still rely on it.
pub fn to_wire_bytes(parts: ResponseParts) -> Vec<u8> {
    let (status, headers, body_bytes, metadata) = parts;
    let validation_errors = if status == 422 {
        hoist::try_hoist_validation_errors(&headers, &body_bytes)
    } else {
        None
    };
    let header_cap = header_capacity_with_floor(&headers, &metadata)
        + validation_errors
            .as_deref()
            .map_or(0, validation_errors_capacity_estimate);
    // `4 + header_cap + body_bytes.len()` cannot overflow `usize` on a
    // 64-bit target (it would require a multi-exabyte body); plain `+` is
    // used so the hot response path keeps its exact arithmetic — a
    // `saturating_add` variant was benchmarked and cost ~2-3% on the small
    // `wire_path`/`request_headers_path` cases for zero real-world benefit.
    // The `validation_errors` term is `0` for every non-422 response (the hot
    // success path is byte-for-byte unchanged); on the 422 path it sizes the
    // `Vec` to serialise the hoisted errors without the mid-write realloc a
    // hoist-blind estimate paid (locked by tests/alloc_budget.rs case F).
    let mut out = Vec::with_capacity(4 + header_cap + body_bytes.len());
    if !write_wire_header_into(
        &mut out,
        status,
        &headers,
        &metadata,
        validation_errors.as_deref(),
    ) {
        // Unreachable for a real `HeaderMap` (would need 4 GiB+ of header
        // JSON); never panic on the response path — emit a 500 instead.
        return error_wire(500, "response header exceeds u32::MAX bytes");
    }
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
    let header_cap = header_capacity_with_floor(headers, metadata);
    let mut out = Vec::with_capacity(4 + header_cap);
    if !write_wire_header_into(&mut out, status, headers, metadata, None) {
        // Unreachable for a real `HeaderMap`; never panic on the response path.
        return error_wire(500, "response header exceeds u32::MAX bytes");
    }
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
    let header_cap = header_capacity_with_floor(headers, metadata)
        + validation_errors
            .as_deref()
            .map_or(0, validation_errors_capacity_estimate);
    let mut out = Vec::with_capacity(4 + header_cap);
    if !write_wire_header_into(
        &mut out,
        status,
        headers,
        metadata,
        validation_errors.as_deref(),
    ) {
        // Unreachable for a real `HeaderMap`; never panic on the response path.
        return error_wire(500, "response header exceeds u32::MAX bytes");
    }
    out
}

/// `io::Write` adapter over a fixed `&mut [u8]`: copies the prefix that
/// fits and *counts* the rest, so a serializer can fill the caller's
/// buffer and still report the exact size it needed on overflow —
/// without allocating or panicking.  `pos` is the running total of bytes
/// the writer was asked to write (it may exceed `buf.len()`).
#[cfg(any(test, feature = "bench-support"))]
struct SliceWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

#[cfg(any(test, feature = "bench-support"))]
impl<'a> SliceWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn put(&mut self, data: &[u8]) {
        if self.pos < self.buf.len() {
            let n = data.len().min(self.buf.len() - self.pos);
            self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
        }
        self.pos += data.len();
    }
}

#[cfg(any(test, feature = "bench-support"))]
impl std::io::Write for SliceWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.put(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Write `[u32 BE header_len | JSON header]` **straight into `out`**
/// with the hand-rolled [`header_write`] serializer, returning the exact
/// total header byte count regardless of whether it fit.  The
/// direct-write sibling of [`build_wire_header_bytes`] — no intermediate
/// `Vec`, byte-identical output to the previous `serde_json` path
/// (retained as [`write_wire_header_into_slice_serde`] for the criterion
/// A/B).
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

/// `serde_json`-backed twin of [`write_wire_header_into_slice`], retained
/// **only** as the "before" arm of the criterion A/B in
/// `benches/dispatch.rs` (via [`crate::bench_support`]) so hand-rolled vs
/// `serde_json` are measured in the same run.  Not part of the public
/// API and not used on any production path.
#[cfg(any(test, feature = "bench-support"))]
fn write_wire_header_into_slice_serde(
    out: &mut [u8],
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> usize {
    let view = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers: &WireHeaders(headers),
        metadata,
        validation_errors: None,
    };
    let header_total = {
        let mut writer = SliceWriter::new(out);
        writer.put(&[0u8; 4]);
        serde_json::to_writer(&mut writer, &view)
            .expect("WireResponseHeader serialization is infallible");
        writer.pos
    };
    if header_total <= out.len() {
        let json_len =
            u32::try_from(header_total - 4).expect("response header JSON exceeds u32::MAX bytes");
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
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&input[..4]);
    let header_len = u32::from_be_bytes(len_bytes) as usize;
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
/// [`parse_wire_header_serde`] for the criterion A/B): any key order,
/// unknown keys ignored, plain strings borrowed / escaped strings owned.
#[inline]
pub fn parse_wire_header(header_json: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    header_read::parse(header_json).map_err(|e| format!("wire header JSON parse error: {e}"))
}

/// `serde_json`-backed twin of [`parse_wire_header`], retained **only**
/// as the "before" arm of the criterion A/B in `benches/dispatch.rs`
/// (via [`crate::bench_support`]) so hand-rolled vs `serde_json` are
/// measured in the same run.  Not part of the public API and not used on
/// any production path.
#[cfg(any(test, feature = "bench-support"))]
fn parse_wire_header_serde(header_json: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    serde_json::from_slice(header_json).map_err(|e| format!("wire header JSON parse error: {e}"))
}

// ── Criterion A/B bench surface (doc-hidden, not a public API) ────────
//
// These thin wrappers expose the hand-rolled and `serde_json` paths to
// `benches/dispatch.rs` (re-exported via `crate::bench_support`) so both
// are measured in the SAME criterion run — the noise-robust same-run A/B
// the existing `direct_write_path/bodyless_*` group uses.  Each parse
// wrapper sums every decoded field length so the optimiser cannot elide
// any field's materialisation (representative of the full production
// parse), and returns a plain `usize` so no borrowed/private type leaks
// into the (hidden) public surface.

/// Bench A/B: full hand-rolled request-header parse cost.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_parse_hand(header_json: &[u8]) -> usize {
    parse_wire_header(header_json).map_or(usize::MAX, |h| header_field_len_sum(&h))
}

/// Bench A/B: full `serde_json` request-header parse cost.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_parse_serde(header_json: &[u8]) -> usize {
    parse_wire_header_serde(header_json).map_or(usize::MAX, |h| header_field_len_sum(&h))
}

/// Sum of every decoded field's byte length — forces materialisation of
/// each `Cow` (UTF-8 validation / escape decode) so neither A/B arm can
/// be optimised down to a partial parse.  Takes the header by reference;
/// the owned value is still dropped inside the timed `bench_parse_*` call.
#[cfg(any(test, feature = "bench-support"))]
fn header_field_len_sum(header: &WireRequestHeader<'_>) -> usize {
    let mut acc = header.method.len()
        + header.path.len()
        + header.query.len()
        + header.app.as_deref().map_or(0, str::len)
        + usize::from(header.v);
    for (name, value) in &header.headers {
        acc += name.len() + value.len();
    }
    acc
}

/// Bench A/B: hand-rolled response-header slice serialize cost.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_write_hand(
    out: &mut [u8],
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> usize {
    write_wire_header_into_slice(out, status, headers, metadata)
}

/// Bench A/B: `serde_json` response-header slice serialize cost.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_write_serde(
    out: &mut [u8],
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> usize {
    write_wire_header_into_slice_serde(out, status, headers, metadata)
}
