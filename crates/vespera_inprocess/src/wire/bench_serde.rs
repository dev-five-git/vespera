//! Bench-only `serde_json` twins of the hand-rolled wire header path.
//!
//! Kept in a sibling module so [`super`] (the production `wire.rs`)
//! documents only the shipped hand-rolled parser/serializer.  Every item
//! here is compiled ONLY under `#[cfg(any(test, feature = "bench-support"))]`
//! and is referenced solely by:
//!
//! - the same-run criterion A/B in `benches/dispatch.rs` (through the
//!   `crate::bench_support::bench_parse_*` / `bench_write_*` re-exports
//!   in [`super`]), and
//! - the round-trip property tests in [`super::tests`]
//!   (`hand_parse_matches_serde_parse`,
//!    `hand_serialize_matches_serde_serialize`,
//!    `hand_serialize_matches_serde_for_tiny_header_maps`).
//!
//! Byte-identical to the retired `serde_json`-only path (locked by
//! `tests/wire_contract.rs` and the round-trip tests above); DO NOT reach
//! any of this from a production code path.
#![cfg(any(test, feature = "bench-support"))]

use std::borrow::Cow;

use serde::Serialize;

use crate::envelope::ResponseMetadata;

use super::header_write::{JsonSink, SliceSink};
use super::{
    CowPairs, STACK_CAP, ValidationErrorItem, WIRE_VERSION, WireRequestHeader, parse_wire_header,
    write_wire_header_into_slice,
};

// ── Borrowed Cow helper for the bench-only serde parse ───────────────

/// `Cow<str>` wrapper whose `Deserialize` impl borrows from the input
/// when the JSON string carries no escape sequences.  Feeds the serde
/// A/B twin; production parsing is hand-rolled ([`super::header_read`]).
pub(super) struct BorrowableCow<'a>(pub(super) Cow<'a, str>);

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

/// Deserialize a JSON object into a flat `Vec` of `(name, value)`
/// pairs whose strings borrow from the input where possible — one
/// `Vec` allocation instead of `HashMap` buckets + per-key hashing.
/// Bench-only (feeds the serde A/B twin).
pub(super) fn de_cow_pairs<'de, D: serde::Deserializer<'de>>(
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
pub(super) fn de_opt_cow<'de, D: serde::Deserializer<'de>>(
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

// ── Serde response-header twins ──────────────────────────────────────

// wire-order locked — field order defines the serialized wire header
// byte layout (`v`, `status`, `headers`, `metadata`,
// `validation_errors?`).  See tests/wire_contract.rs.
#[derive(Serialize)]
pub(super) struct WireResponseHeader<'a, H: Serialize> {
    pub(super) v: u8,
    pub(super) status: u16,
    pub(super) headers: &'a H,
    pub(super) metadata: &'a ResponseMetadata,
    /// Validation errors hoisted from a 422 JSON body so Java decoders
    /// can read them with a single header parse.  `None` for any other
    /// status; the original body is preserved verbatim regardless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) validation_errors: Option<Vec<ValidationErrorItem>>,
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
pub(super) struct WireHeaders<'a>(pub(super) &'a http::HeaderMap);

impl Serialize for WireHeaders<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // `HeaderMap::keys` yields each distinct name exactly once.  The
        // overwhelmingly common response carries only a handful of header
        // names, so sort them in a stack buffer and skip the per-response
        // heap `Vec`; header sets larger than the stack cap fall back to a
        // heap `Vec`.  Output is byte-identical either way (same sorted
        // order over the same names), as locked by tests/wire_contract.rs.
        // `STACK_CAP` is shared with the production `write_headers` in
        // [`super::header_write`] so both arms of the same-run
        // `wire_header_serde` criterion A/B stay locked to the same cap.
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
struct WireHeaderValues<'a>(&'a http::HeaderMap, &'a str);

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

// ── Serde parse/serialize twins used by the criterion A/B ────────────

/// `serde_json`-backed twin of [`super::write_wire_header_into_slice`],
/// retained **only** as the "before" arm of the criterion A/B in
/// `benches/dispatch.rs` (via [`crate::bench_support`]) so hand-rolled vs
/// `serde_json` are measured in the same run.  Not part of the public
/// API and not used on any production path.
pub(super) fn write_wire_header_into_slice_serde(
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
        let mut writer = SliceSink::new(out);
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

/// `serde_json`-backed twin of [`super::parse_wire_header`], retained
/// **only** as the "before" arm of the criterion A/B in
/// `benches/dispatch.rs` (via [`crate::bench_support`]) so hand-rolled vs
/// `serde_json` are measured in the same run.  Not part of the public
/// API and not used on any production path.
pub(super) fn parse_wire_header_serde(header_json: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    serde_json::from_slice(header_json).map_err(|e| format!("wire header JSON parse error: {e}"))
}

// ── Criterion A/B bench surface (doc-hidden, not a public API) ──────
//
// These thin wrappers expose the hand-rolled and `serde_json` paths to
// `benches/dispatch.rs` (re-exported via `crate::bench_support` through
// [`super`]) so both are measured in the SAME criterion run — the
// noise-robust same-run A/B the existing `direct_write_path/bodyless_*`
// group uses.  Each parse wrapper sums every decoded field length so the
// optimiser cannot elide any field's materialisation (representative of
// the full production parse), and returns a plain `usize` so no
// borrowed/private type leaks into the (hidden) public surface.

/// Bench A/B: full hand-rolled request-header parse cost.
#[doc(hidden)]
#[must_use]
pub fn bench_parse_hand(header_json: &[u8]) -> usize {
    parse_wire_header(header_json).map_or(usize::MAX, |h| header_field_len_sum(&h))
}

/// Bench A/B: full `serde_json` request-header parse cost.
#[doc(hidden)]
#[must_use]
pub fn bench_parse_serde(header_json: &[u8]) -> usize {
    parse_wire_header_serde(header_json).map_or(usize::MAX, |h| header_field_len_sum(&h))
}

/// Sum of every decoded field's byte length — forces materialisation of
/// each `Cow` (UTF-8 validation / escape decode) so neither A/B arm can
/// be optimised down to a partial parse.  Takes the header by reference;
/// the owned value is still dropped inside the timed `bench_parse_*` call.
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
