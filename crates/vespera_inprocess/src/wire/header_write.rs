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

/// Append the decimal representation of `v` (no leading zeros, `0` for
/// zero) — byte-identical to `serde_json`'s `itoa` integer output for
/// the `u8`/`u16` header fields.
fn write_u64<S: JsonSink>(sink: &mut S, mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + u8::try_from(v % 10).unwrap_or(0);
        v /= 10;
        if v == 0 {
            break;
        }
    }
    sink.put(&buf[i..]);
}

/// Serialize an [`http::HeaderMap`] as the wire's sorted name -> value
/// JSON map — byte-compatible with [`super::WireHeaders`]:
/// - names sort in byte order (`HeaderName`s are lowercase ASCII, so
///   `sort_unstable` equals the prior `BTreeMap` ordering);
/// - single-valued headers render as a JSON string, repeated names as a
///   JSON array in insertion order;
/// - non-UTF-8 values render as `""` (same `to_str().unwrap_or("")`).
fn write_headers<S: JsonSink>(sink: &mut S, headers: &http::HeaderMap) {
    // Sort distinct names in a stack buffer for the common small-header
    // response; larger sets fall back to a heap `Vec`.  Output is
    // byte-identical either way (same sorted order over the same names).
    const STACK_CAP: usize = 32;
    let key_count = headers.keys_len();
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
        write_json_string(sink, name);
        sink.put(b":");
        let mut values = headers.get_all(name).iter();
        let first = values
            .next()
            .expect("HeaderMap::keys yields only present names");
        match values.next() {
            // Single value: emit the scalar string.
            None => write_json_string(sink, first.to_str().unwrap_or("")),
            // Multiple values: emit a JSON array, reusing the already
            // advanced iterator (first, second, then the rest) instead of
            // re-iterating `get_all(name)` from the start — byte-identical
            // output, no second hash lookup, important for repeated
            // headers like `set-cookie`.
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
    sink.put(b"}");
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
    sink.put(b"{\"v\":");
    write_u64(sink, u64::from(WIRE_VERSION));
    sink.put(b",\"status\":");
    write_u64(sink, u64::from(status));
    sink.put(b",\"headers\":");
    write_headers(sink, headers);
    // COUPLING: this hand-written `metadata` object mirrors
    // `ResponseMetadata`'s serde shape field-for-field.  Adding a
    // serialized field to `ResponseMetadata` (envelope.rs) without
    // updating this line breaks the byte-identity guard
    // `hand_serialize_matches_serde_serialize` (wire/tests.rs) — that test
    // is the drift tripwire, so keep the two in lockstep.
    sink.put(b",\"metadata\":{\"version\":");
    write_json_string(sink, &metadata.version);
    sink.put(b"}");
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
