//! 422 validation-error hoisting, split out to keep `wire.rs` under the
//! 1000-line cap.  Pure code move: no logic or byte-behaviour change.

use bytes::Bytes;
use serde::Deserialize;

use super::ValidationErrorItem;

/// Upper bound on a `422` response body that [`try_hoist_validation_errors`]
/// will reparse to hoist validation errors into the wire header.  A
/// canonical validation envelope is at most a few KiB even with many field
/// errors; beyond this the (cold-path) hoist is skipped and the body is
/// surfaced verbatim, so a large 422 body never forces a full
/// `serde_json::Value` reparse.
const MAX_HOIST_BODY_BYTES: usize = 64 * 1024;

/// First content-type value decides whether a 422 body is JSON for the
/// validation-error hoist (matches the previous first-of-`Multi`
/// behaviour).  Comparisons are case-insensitive in place — no
/// lowercased copy.
fn body_is_json(headers: &http::HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            // Any `application/json`, `*/json`, or `*+json` media type. The
            // trailing-5-byte suffix is compared on raw bytes (not a `str`
            // slice), so an exotic non-ASCII value can never panic on a
            // non-char-boundary index — and `/json` (e.g. `text/json`) now
            // hoists too, matching the documented contract.
            let mime = s.split(';').next().unwrap_or("").trim().as_bytes();
            mime.len() >= 5 && {
                let suffix = &mime[mime.len() - 5..];
                suffix.eq_ignore_ascii_case(b"/json") || suffix.eq_ignore_ascii_case(b"+json")
            }
        })
}

/// Typed shape of the validation envelope, deserialized **directly** from the
/// 422 body — skips building the intermediate `serde_json::Value` DOM (the
/// object map + array vec + per-error maps + interned string keys) the
/// previous reparse allocated, going straight to the `Vec<HoistErrorIn>` whose
/// owned strings [`ValidationErrorItem`] needs anyway.
///
/// This is the **fast strict path**: the common, framework-generated envelope
/// has all-string fields, so the plain derive parses it with no per-field
/// visitor overhead.  A body with a wrong-typed field (`"code": 123`) fails
/// this strict parse and is retried via [`LenientHoistEnvelope`], so the
/// hoist stays genuinely best-effort without taxing the common case.
#[derive(Deserialize)]
struct HoistEnvelope {
    errors: Vec<HoistErrorIn>,
}

#[derive(Deserialize)]
struct HoistErrorIn {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Deserialize an optional string **leniently**: a JSON string yields
/// `Some`, while `null` / a missing field / any non-string value (number,
/// bool, object, array) yields `None` instead of failing the parse.  This
/// keeps the 422 hoist genuinely *best-effort* — a single odd error object
/// (e.g. `{"code": 123}`) never aborts the whole hoist, matching the
/// documented contract and the previous `serde_json::Value` extract path
/// (`e.get("code").and_then(Value::as_str)`).  Zero-allocation: a wrong-typed
/// scalar is dropped without building a `Value` DOM.
fn de_lenient_opt_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string, null, or any JSON value")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_borrowed_str<E: serde::de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        // Anything that is not a JSON string → `None` (best-effort, never err).
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2: serde::Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(self)
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i128<E: serde::de::Error>(self, _: i128) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u128<E: serde::de::Error>(self, _: u128) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            while access
                .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                .is_some()
            {}
            Ok(None)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }
    deserializer.deserialize_any(V)
}

/// Lenient fallback shape, parsed **only** when the strict [`HoistEnvelope`]
/// parse fails on a wrong-typed field.  Each field decodes through
/// [`de_lenient_opt_string`], so a hand-crafted 422 body like
/// `{"errors":[{"path":"a","code":123}]}` still hoists every entry that has a
/// usable `path`.  Confined to this cold retry so the common all-string
/// envelope never pays the per-field visitor cost.
#[derive(Deserialize)]
struct LenientHoistEnvelope {
    errors: Vec<LenientHoistErrorIn>,
}

#[derive(Deserialize)]
struct LenientHoistErrorIn {
    #[serde(default, deserialize_with = "de_lenient_opt_string")]
    path: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_opt_string")]
    code: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_opt_string")]
    message: Option<String>,
}

/// Collect hoistable `(path, code, message)` triples into wire items,
/// skipping any error that lacks a usable `path` (matches the previous
/// `e.get("path")?.as_str()?` behaviour).  Shared by the strict fast path
/// and the lenient fallback so both apply identical selection rules.
fn hoist_items(
    errors: impl Iterator<Item = (Option<String>, Option<String>, Option<String>)>,
) -> Vec<ValidationErrorItem> {
    errors
        .filter_map(|(path, code, message)| {
            Some(ValidationErrorItem {
                path: path?,
                code,
                message,
            })
        })
        .collect()
}

/// Best-effort extract validation errors from a 422 JSON body.
///
/// Returns `None` (silently) for:
/// - non-JSON content-types (anything that doesn't end in `/json` or
///   `+json`)
/// - body bytes that don't parse as the `{"errors":[...]}` envelope
/// - an envelope whose hoistable errors (those carrying a `path`) are empty
///
/// This is intentionally lenient — a malformed 422 body must never
/// degrade to a 5xx; the original body is still surfaced verbatim.
pub(super) fn try_hoist_validation_errors(
    headers: &http::HeaderMap,
    body_bytes: &Bytes,
) -> Option<Vec<ValidationErrorItem>> {
    if !body_is_json(headers) {
        return None;
    }
    // Cold-path guard: a 422 validation envelope is framework-generated and
    // tiny.  For an unexpectedly large body, skip the parse + per-item owned
    // allocations rather than churning heap on it; the original body is still
    // surfaced verbatim on the wire.
    if body_bytes.len() > MAX_HOIST_BODY_BYTES {
        return None;
    }
    // Fast path: strict typed deserialize (no intermediate `serde_json::Value`
    // DOM, no per-field visitor) — the common all-string framework envelope
    // parses here directly.
    let items = if let Ok(envelope) = serde_json::from_slice::<HoistEnvelope>(body_bytes) {
        hoist_items(
            envelope
                .errors
                .into_iter()
                .map(|e| (e.path, e.code, e.message)),
        )
    } else {
        // A wrong-typed field aborted the strict parse; retry leniently so a
        // single odd error object never loses the other valid errors. Cold
        // (only a hand-crafted 422 body reaches here), so the second parse of
        // the already-size-capped body is negligible.
        let envelope: LenientHoistEnvelope = serde_json::from_slice(body_bytes).ok()?;
        hoist_items(
            envelope
                .errors
                .into_iter()
                .map(|e| (e.path, e.code, e.message)),
        )
    };
    if items.is_empty() { None } else { Some(items) }
}

/// **Bench-only** `serde_json::Value` twin of [`try_hoist_validation_errors`],
/// retained as the "before" arm of the `hoist_422_ab` criterion A/B
/// (same-run, noise-robust — mirroring the `wire_header_serde` /
/// `request_build_ab` twins).  Parses the body into a full `Value` DOM then
/// re-extracts each field — the allocation-heavier path the typed deserialize
/// replaced; byte-identical result for the framework-generated envelope.  Not
/// used on any production path.
#[cfg(any(test, feature = "bench-support"))]
fn try_hoist_validation_errors_value_old(
    headers: &http::HeaderMap,
    body_bytes: &Bytes,
) -> Option<Vec<ValidationErrorItem>> {
    if !body_is_json(headers) {
        return None;
    }
    if body_bytes.len() > MAX_HOIST_BODY_BYTES {
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

/// Sum every hoisted item's field byte lengths so neither `hoist_422_ab` arm
/// can be optimised down to a partial parse.  `None` (no hoist) sums to 0.
#[cfg(any(test, feature = "bench-support"))]
fn hoist_field_len_sum(items: Option<Vec<ValidationErrorItem>>) -> usize {
    items.map_or(0, |v| {
        v.iter()
            .map(|i| {
                i.path.len()
                    + i.code.as_deref().map_or(0, str::len)
                    + i.message.as_deref().map_or(0, str::len)
            })
            .sum()
    })
}

/// Bench A/B: production typed-deserialize 422 validation hoist cost.
/// Bench-only.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_hoist_new(headers: &http::HeaderMap, body: &Bytes) -> usize {
    hoist_field_len_sum(try_hoist_validation_errors(headers, body))
}

/// Bench A/B: previous `serde_json::Value` DOM 422 validation hoist cost.
/// Bench-only.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_hoist_old(headers: &http::HeaderMap, body: &Bytes) -> usize {
    hoist_field_len_sum(try_hoist_validation_errors_value_old(headers, body))
}
