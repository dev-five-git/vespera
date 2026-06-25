//! Hand-rolled serializer for the **fixed-schema response wire header**
//! — the byte-for-byte replacement for the `serde_json::to_writer` path
//! that used to render [`super::WireResponseHeader`].
//!
//! Output is **byte-identical** to `serde_json`'s compact serialization
//! (locked by `tests/wire_contract.rs` and the in-crate round-trip
//! property test in `wire.rs`):
//!
//! ```text
//! {"v":1,"status":<u16>,"headers":<sorted map>,
//!  "metadata":{"version":"<v>"}[,"validation_errors":[...]]}
//! ```
//!
//! The string escaper reproduces exactly the set `serde_json` (and the
//! Java `VesperaBridge.writeJsonString`) emit: only `"`, `\`, and the
//! C0 controls (`\b \t \n \f \r`, else `\u00XX` in lowercase hex) are
//! escaped; `/` and 0x7F pass through, and every byte `>= 0x80` is
//! copied through verbatim (raw UTF-8).

use std::borrow::Cow;

use crate::envelope::ResponseMetadata;

use super::{ValidationErrorItem, WIRE_VERSION};

/// Byte sink abstraction so one serializer serves both the growable
/// `Vec<u8>` path ([`super::write_wire_header_into`]) and the fixed
/// `&mut [u8]` direct-write path ([`super::write_wire_header_into_slice`],
/// which copies the prefix that fits and counts the overflow).
pub(super) trait JsonSink {
    fn put(&mut self, data: &[u8]);
}

impl JsonSink for Vec<u8> {
    #[inline]
    fn put(&mut self, data: &[u8]) {
        self.extend_from_slice(data);
    }
}

/// Fixed-slice sink: copies the prefix that fits into `buf` and *counts*
/// the rest, so the caller can report the exact size needed on overflow
/// without allocating or panicking.  `pos` is the running total of bytes
/// the serializer asked to write (it may exceed `buf.len()`) — the
/// direct-write `Overflow` contract.
pub(super) struct SliceSink<'a> {
    buf: &'a mut [u8],
    pub(super) pos: usize,
}

impl<'a> SliceSink<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl JsonSink for SliceSink<'_> {
    #[inline]
    fn put(&mut self, data: &[u8]) {
        if self.pos < self.buf.len() {
            let n = data.len().min(self.buf.len() - self.pos);
            self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
        }
        self.pos += data.len();
    }
}

// ── serde_json-exact string escaping ─────────────────────────────────
//
// Reproduces `serde_json`'s `ESCAPE` lookup table + `write_char_escape`
// byte-for-byte: index by source byte, `0` means "copy verbatim",
// anything else selects an escape.  Identical to the table the Java
// `writeJsonString` encodes by hand.

const BB: u8 = b'b'; // \x08 -> \b
const TT: u8 = b't'; // \x09 -> \t
const NN: u8 = b'n'; // \x0A -> \n
const FF: u8 = b'f'; // \x0C -> \f
const RR: u8 = b'r'; // \x0D -> \r
const QU: u8 = b'"'; // \x22 -> \"
const BS: u8 = b'\\'; // \x5C -> \\
const UU: u8 = b'u'; // other C0 control -> \u00XX
const XX: u8 = 0; // verbatim (no escape)

#[rustfmt::skip]
static ESCAPE: [u8; 256] = [
    //  0    1    2    3    4    5    6    7    8    9    A    B    C    D    E    F
    UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  BB,  TT,  NN,  UU,  FF,  RR,  UU,  UU, // 0
    UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU,  UU, // 1
    XX,  XX,  QU,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 2
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 3
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 4
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  BS,  XX,  XX,  XX, // 5
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 6
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 7
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 8
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // 9
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // A
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // B
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // C
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // D
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // E
    XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX,  XX, // F
];

const HEX: &[u8; 16] = b"0123456789abcdef";

// ── pre-baked metadata segment for the production fast path ──────────
//
// `ResponseMetadata::current()` ([`crate::envelope`]) — the version constructor
// every production response producer reaches for — stores
// `Cow::Borrowed(env!("CARGO_PKG_VERSION"))`.  That `'static` string is also
// the only value the byte-identity guard `hand_serialize_matches_serde_serialize`
// in `wire/tests.rs` exercises through the current() path.
//
// SemVer (`[0-9a-zA-Z.+-]`) never trips the `ESCAPE` table, so
// `write_json_string` over `metadata.version` here would emit exactly the
// bytes already baked into the const below.  Skipping the per-byte escape
// scan + the two surrounding `sink.put` calls saves ~5-10 ESCAPE lookups
// + 3 indirect sink puts on EVERY wire response (buffered / direct-write /
// streaming) when the metadata is `current()`.  Manually-constructed
// `ResponseMetadata` (tests, custom callers) or any `Cow::Owned` version
// falls through to the general path, so the slow-path output is byte-for-byte
// unchanged for non-`current()` inputs.

/// Engine version — the `&'static str` `ResponseMetadata::current()` stores
/// in its `version` field.  Used by the production fast path below to
/// pointer-eq detect that exact constructor at zero runtime cost.
const VESPERA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Pre-baked `,"metadata":{"version":"X.Y.Z"}` segment for the production
/// fast path — byte-identical to what the general `write_json_string` path
/// produces for `ResponseMetadata::current()`'s SemVer version (SemVer is
/// drawn from `[0-9a-zA-Z.+-]`, none of which trip the `ESCAPE` table).
/// Emitted in one bulk `sink.put` instead of three calls + a per-byte scan
/// over the same string.
const METADATA_SEGMENT_CURRENT: &[u8] = concat!(
    ",\"metadata\":{\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\"}"
)
.as_bytes();

/// Append `s` as a quoted, escaped JSON string straight into `sink` —
/// the byte-for-byte analogue of `serde_json`'s `format_escaped_str`.
/// Runs of non-escaped bytes are copied in bulk; only the escape set
/// above is rewritten.
fn write_json_string<S: JsonSink>(sink: &mut S, s: &str) {
    sink.put(b"\"");
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == XX {
            continue;
        }
        if start < i {
            sink.put(&bytes[start..i]);
        }
        match escape {
            BB => sink.put(b"\\b"),
            TT => sink.put(b"\\t"),
            NN => sink.put(b"\\n"),
            FF => sink.put(b"\\f"),
            RR => sink.put(b"\\r"),
            QU => sink.put(b"\\\""),
            BS => sink.put(b"\\\\"),
            // `UU`: a C0 control with no short form -> `\u00XX` (lowercase hex).
            _ => sink.put(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(byte >> 4) as usize],
                HEX[(byte & 0xF) as usize],
            ]),
        }
        start = i + 1;
    }
    if start < bytes.len() {
        sink.put(&bytes[start..]);
    }
    sink.put(b"\"");
}

/// Append a 3-digit HTTP status code (100-999) as ASCII — the
/// byte-identical replacement for the prior generic
/// `write_u64(sink, u64::from(status))` call.  `http::StatusCode`
/// enforces the 100..=999 range (`axum::response::Response::status()`
/// returns it), so the loop, the 20-byte stack buffer, and the
/// `u16 → u64` widening the generic writer carried are all redundant
/// for the sole caller.  Mirrors the Rust-side fast paths in
/// `write_headers` (0/1-key) and the Java-side inlined-digit pattern
/// in `VesperaWireCodec.java::fillHeaderJson` (`'0' + WIRE_VERSION`).
/// Byte-identical output — locked by `hand_serialize_matches_serde_serialize`
/// in `wire/tests.rs` and the end-to-end `tests/wire_contract.rs`.
fn write_status_code<S: JsonSink>(sink: &mut S, status: u16) {
    debug_assert!(
        (100..=999).contains(&status),
        "HTTP status must be 100..=999"
    );
    // Each digit is bounded to 0..=9 (1..=9 for `hundreds`) so the `u8`
    // conversion never truncates in practice; `u8::try_from` is the
    // pedantic-clippy-clean form (matches the original `write_u64`'s
    // `u8::try_from(v % 10).unwrap_or(0)` and compiles to the same
    // no-op truncation in release builds).
    let hundreds = u8::try_from(status / 100).unwrap_or(0);
    let tens = u8::try_from((status / 10) % 10).unwrap_or(0);
    let ones = u8::try_from(status % 10).unwrap_or(0);
    sink.put(&[b'0' + hundreds, b'0' + tens, b'0' + ones]);
}

/// Serialize an [`http::HeaderMap`] as the wire's sorted name -> value
/// JSON map — byte-compatible with [`super::WireHeaders`]:
/// - names sort in byte order (`HeaderName`s are lowercase ASCII, so
///   `sort_unstable` equals the prior `BTreeMap` ordering);
/// - single-valued headers render as a JSON string, repeated names as a
///   JSON array in insertion order;
/// - non-UTF-8 values render as `""` (same `to_str().unwrap_or("")`).
fn write_headers<S: JsonSink>(sink: &mut S, headers: &http::HeaderMap) {
    const STACK_CAP: usize = 32;
    let key_count = headers.keys_len();
    // Fast paths for the overwhelmingly common tiny-header responses: skip
    // initialising the 32-slot stack name array AND the (no-op) sort that the
    // general path below always pays — a bodyless `GET` returning a bare string
    // has ZERO response headers, and a single `content-type` is the next most
    // common shape.  Output is byte-identical (an N<=1 set is trivially sorted).
    if key_count == 0 {
        sink.put(b"{}");
        return;
    }
    if key_count == 1 {
        // `keys_len() == 1` guarantees exactly one key.  On the impossible
        // `None` we emit an empty object instead of panicking, keeping this
        // FFI-adjacent response serializer free of unwind sites (mirrors the
        // no-panic/unwind discipline the dispatch internals document).
        if let Some(name) = headers.keys().next() {
            sink.put(b"{");
            write_header_name_json_string(sink, name.as_str());
            sink.put(b":");
            write_header_value(sink, headers, name.as_str());
            sink.put(b"}");
        } else {
            debug_assert!(false, "keys_len()==1 yields exactly one name");
            sink.put(b"{}");
        }
        return;
    }

    // >=2 distinct names: sort in a stack buffer for the common small-header
    // response; larger sets fall back to a heap `Vec`.  Output is byte-identical
    // either way (same sorted order over the same names).
    let mut stack_names: [&str; STACK_CAP] = [""; STACK_CAP];
    let mut heap_names: Vec<&str>;
    let names: &mut [&str] = if key_count <= STACK_CAP {
        for (slot, name) in stack_names.iter_mut().zip(headers.keys()) {
            *slot = name.as_str();
        }
        &mut stack_names[..key_count]
    } else {
        heap_names = Vec::with_capacity(key_count);
        heap_names.extend(headers.keys().map(http::HeaderName::as_str));
        &mut heap_names[..]
    };
    names.sort_unstable();

    sink.put(b"{");
    for (idx, &name) in names.iter().enumerate() {
        if idx > 0 {
            sink.put(b",");
        }
        write_header_name_json_string(sink, name);
        sink.put(b":");
        write_header_value(sink, headers, name);
    }
    sink.put(b"}");
}

/// Append an HTTP header **name** as a quoted JSON string WITHOUT the
/// escape-table scan.  An `http::HeaderName` is a validated HTTP field-name
/// token (RFC 9110 §5.6.2 — only `!#$%&'*+-.^_`|~`, digits, and ASCII letters,
/// lowercase here), so it can contain NONE of the `"`, `\`, or C0-control bytes
/// `write_json_string` rewrites.  Byte-identical to `write_json_string(sink,
/// name)` for any valid header name, but skips the per-byte escape lookup.
fn write_header_name_json_string<S: JsonSink>(sink: &mut S, name: &str) {
    sink.put(b"\"");
    sink.put(name.as_bytes());
    sink.put(b"\"");
}

/// Write the JSON value for header `name`: a scalar string for a single value,
/// or a JSON array (insertion order) for a repeated name (e.g. `set-cookie`).
/// Reuses the already-advanced `get_all` iterator for the multi-value case
/// (first, second, then the rest) — byte-identical, no second hash lookup.
fn write_header_value<S: JsonSink>(sink: &mut S, headers: &http::HeaderMap, name: &str) {
    let mut values = headers.get_all(name).iter();
    // `write_header_value` is only invoked for names taken from `headers.keys()`,
    // so `get_all(name)` is always non-empty.  On the impossible `None` we emit an
    // empty-string value rather than panicking on this response hot path.
    let Some(first) = values.next() else {
        debug_assert!(false, "write_header_value is only called for present names");
        write_json_string(sink, "");
        return;
    };
    match values.next() {
        // Single value: emit the scalar string.
        None => write_json_string(sink, first.to_str().unwrap_or("")),
        // Multiple values: emit a JSON array.
        Some(second) => {
            sink.put(b"[");
            write_json_string(sink, first.to_str().unwrap_or(""));
            sink.put(b",");
            write_json_string(sink, second.to_str().unwrap_or(""));
            for value in values {
                sink.put(b",");
                write_json_string(sink, value.to_str().unwrap_or(""));
            }
            sink.put(b"]");
        }
    }
}

/// Serialize one `validation_errors` entry — fields in struct order
/// (`path`, then `code`/`message` when present), matching the
/// `#[serde(skip_serializing_if = "Option::is_none")]` derive.
fn write_validation_item<S: JsonSink>(sink: &mut S, item: &ValidationErrorItem) {
    sink.put(b"{\"path\":");
    write_json_string(sink, &item.path);
    if let Some(code) = &item.code {
        sink.put(b",\"code\":");
        write_json_string(sink, code);
    }
    if let Some(message) = &item.message {
        sink.put(b",\"message\":");
        write_json_string(sink, message);
    }
    sink.put(b"}");
}

/// Serialize the full response wire header into `sink` (no length
/// prefix) — the byte-for-byte replacement for
/// `serde_json::to_writer(WireResponseHeader { .. })`.  Field order is
/// locked: `v`, `status`, `headers`, `metadata`, optional
/// `validation_errors`.
pub(super) fn write_response_header<S: JsonSink>(
    sink: &mut S,
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
    validation_errors: Option<&[ValidationErrorItem]>,
) {
    // `WIRE_VERSION` is a single-digit constant; write its ASCII digit
    // directly in the scaffolding literal to avoid the per-response
    // `write_u64` digit-extraction loop and one `JsonSink::put` indirection
    // every wire response (buffered / direct-write / streaming) used to pay.
    // Byte-identical output — locked by `hand_serialize_matches_serde_serialize`
    // in `wire/tests.rs` and the end-to-end `tests/wire_contract.rs`.
    // Mirrors the Java request-side encoder in `VesperaWireCodec.java::fillHeaderJson`
    // which already inlines `'0' + WIRE_VERSION` at the encode hot path.
    // The const assertion is the safety pin: bumping `WIRE_VERSION` past 1
    // fails to compile here with a clear pointer to the inlined literal that
    // needs updating in lockstep — same compile-time discipline the
    // wire-contract test uses to lock the envelope shape.
    const _: () = assert!(WIRE_VERSION == 1);
    sink.put(b"{\"v\":1,\"status\":");
    write_status_code(sink, status);
    sink.put(b",\"headers\":");
    write_headers(sink, headers);
    // COUPLING: this hand-written `metadata` object mirrors
    // `ResponseMetadata`'s serde shape field-for-field.  Adding a
    // serialized field to `ResponseMetadata` (envelope.rs) without
    // updating this line breaks the byte-identity guard
    // `hand_serialize_matches_serde_serialize` (wire/tests.rs) — that test
    // is the drift tripwire, so keep the two in lockstep.
    //
    // Fast path: every production response producer constructs
    // `ResponseMetadata::current()`, whose `version` is
    // `Cow::Borrowed(env!("CARGO_PKG_VERSION"))` — the same `'static`
    // pointer as `VESPERA_VERSION` (string-literal deduplication within
    // this crate).  Detect that exact constructor by pointer-eq and emit
    // the pre-baked segment in one `sink.put`; everything else (owned
    // versions, manually-constructed metadata in tests / custom callers)
    // falls through to the unchanged general path.  Byte-identity for the
    // fast path is locked by `hand_serialize_matches_serde_serialize` in
    // `wire/tests.rs` (which writes `ResponseMetadata::current()` through
    // both hand and serde and asserts byte equality).
    match &metadata.version {
        Cow::Borrowed(v) if std::ptr::eq(v.as_ptr(), VESPERA_VERSION.as_ptr()) => {
            // Defense in depth: in debug builds, assert byte-identity so a
            // (theoretical) compiler that fails to dedupe the literal still
            // catches any drift via tests.  The slow path below would
            // produce byte-identical output anyway — this only triggers if
            // pointer-eq somehow held on differing bytes, which it cannot.
            debug_assert_eq!(*v, VESPERA_VERSION);
            sink.put(METADATA_SEGMENT_CURRENT);
        }
        _ => {
            sink.put(b",\"metadata\":{\"version\":");
            write_json_string(sink, &metadata.version);
            sink.put(b"}");
        }
    }
    if let Some(items) = validation_errors {
        sink.put(b",\"validation_errors\":[");
        for (idx, item) in items.iter().enumerate() {
            if idx > 0 {
                sink.put(b",");
            }
            write_validation_item(sink, item);
        }
        sink.put(b"]");
    }
    sink.put(b"}");
}
