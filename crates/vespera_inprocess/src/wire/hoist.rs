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
            //
            // `split_once(';')` returns `Some((head, _))` when a parameter
            // list is present (`application/json; charset=utf-8`) and
            // `None` when it isn't (`application/json`), so the media type
            // is the head slice or the whole string — no dead `Split::next`
            // fallback (`.next()` on `&str` never yields `None`).
            let mime = s
                .split_once(';')
                .map_or(s, |(head, _)| head)
                .trim()
                .as_bytes();
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
/// this strict parse and is retried via the inline `serde_json::Value`
/// fallback walk in [`try_hoist_validation_errors`], so the hoist stays
/// genuinely best-effort without taxing the common case.
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

/// Lenient `serde_json::Value` walk over a 422 body: parse the DOM, read the
/// `errors` key, coerce it to an array, and extract `path`/`code`/`message`
/// with `as_str` (wrong types → `None`), SKIPPING any element lacking a usable
/// `path` (non-objects: `Value::get` returns `None`).  This keeps every
/// still-valid error instead of discarding the whole array when one entry is
/// malformed — the bug a typed `Vec<Struct>` fallback had, since one bad
/// element failed the entire `Vec` deserialize.
///
/// `None` means the body isn't a JSON document with an `errors` array at all;
/// `Some(vec![])` means it is, but nothing in it was hoistable.  Callers apply
/// the empty→`None` collapse themselves.
///
/// Shared verbatim by the production fallback in
/// [`try_hoist_validation_errors`] and the bench-only
/// `try_hoist_validation_errors_value_old` twin, so both arms stay
/// accept-identical and the hoisted envelope stays byte-identical.  Only the
/// walk is shared — never the strict fast-path dispatch above it, which the
/// `value_old` twin must keep paying the `Value` DOM without.
fn hoist_from_value(body_bytes: &Bytes) -> Option<Vec<ValidationErrorItem>> {
    let parsed: serde_json::Value = serde_json::from_slice(body_bytes).ok()?;
    let errors = parsed.get("errors")?.as_array()?;
    Some(hoist_items(errors.iter().map(|e| {
        let field = |key| {
            e.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        (field("path"), field("code"), field("message"))
    })))
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
        // The strict typed parse aborted — either a wrong-typed field
        // (`"code": 123`) OR a non-object array element (`null`, a bare
        // string). Retry through the lenient [`hoist_from_value`] walk, which
        // keeps every still-valid error. Cold (only a hand-crafted 422 body
        // reaches here) and size-capped at `MAX_HOIST_BODY_BYTES`, so the
        // `Value` DOM here is negligible and never touches the common
        // all-string fast path.
        hoist_from_value(body_bytes)?
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
    // No strict fast path here, by design: this arm must ALWAYS pay the
    // `serde_json::Value` DOM so the A/B measures exactly what the typed
    // deserialize replaced.
    let items = hoist_from_value(body_bytes)?;
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
