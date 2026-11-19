//! 422 validation-error hoisting, split out of `wire.rs` (1000-line cap):
//! the strict typed fast path plus the lenient raw-JSON fallback.

use std::borrow::Cow;
use std::collections::BTreeMap;

use bytes::Bytes;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::ValidationErrorItem;

/// Upper bound on a `422` response body that [`try_hoist_validation_errors`]
/// will reparse to hoist validation errors into the wire header.  A
/// canonical validation envelope is at most a few KiB even with many field
/// errors; beyond this the (cold-path) hoist is skipped and the body is
/// surfaced verbatim, so a large 422 body never forces a full
/// `serde_json::Value` reparse.
pub const MAX_HOIST_BODY_BYTES: usize = 64 * 1024;

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
/// this strict parse and is retried via the lenient raw-JSON fallback walk
/// ([`hoist_from_raw`]), so the hoist stays
/// genuinely best-effort without taxing the common case.
///
/// Unknown values go through [`SkippedValue`]: it skips the value without
/// converting it (so an out-of-range number such as `1e400` is accepted, as
/// `IgnoredAny` accepts it) but still validates every `\u` surrogate pairing
/// in its strings, which `IgnoredAny` does not and the `Value` walk does.  A
/// lone surrogate therefore fails the strict parse and the body is rejected
/// by both paths, while an out-of-range number no longer costs the hoist.
///
/// Both `Deserialize` impls are hand-written **map-only** visitors: a derive
/// would also accept a JSON array through `visit_seq` (positional
/// `[path, code, message]` / `[errors]`), hoisting fields the lenient `Value`
/// walk never sees.  An array now fails the strict parse and falls through to
/// [`hoist_from_value`], which skips it — the two paths stay accept-identical.
struct HoistEnvelope<'a> {
    errors: Vec<HoistErrorIn<'a>>,
}

struct HoistErrorIn<'a> {
    path: Option<&'a str>,
    code: Option<&'a str>,
    message: Option<&'a str>,
}

/// One error field as read from the map: outer `None` = key absent, inner
/// `None` = explicit `null` (both hoist as an absent field).
type Opt<'a> = Option<Option<&'a str>>;

/// Store `slot`'s value, rejecting a repeated key like the derive did (the
/// body then retries through the `Value` walk, where the last key wins).
fn set_once<'de, A: serde::de::MapAccess<'de>, T: Deserialize<'de>>(
    access: &mut A,
    slot: &mut Option<T>,
    name: &'static str,
) -> Result<(), A::Error> {
    if slot.is_some() {
        return Err(serde::de::Error::duplicate_field(name));
    }
    *slot = Some(access.next_value()?);
    Ok(())
}

/// An unknown value, skipped without converting it.  The raw text is captured
/// (`RawValue`, so no number is converted and an out-of-range `1e400` is fine)
/// and checked by [`surrogates_paired`]: a `\u` escape pair must be valid
/// UTF-16, the rule the `serde_json::Value` walk applies to strings.
struct SkippedValue;

impl<'de> Deserialize<'de> for SkippedValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <&serde_json::value::RawValue as Deserialize>::deserialize(deserializer)?;
        if surrogates_paired(raw.get().as_bytes()) {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "lone surrogate in an unknown value",
            ))
        }
    }
}

/// `true` when every `\u` escape in `raw` (a syntactically valid JSON value)
/// is a scalar value or a high surrogate directly followed by its low
/// surrogate.  A backslash in valid JSON always opens an escape, so the scan
/// steps over each escape whole (`\\` included).
fn surrogates_paired(raw: &[u8]) -> bool {
    // A high surrogate was the previous escape, so the next one must be a low
    // `\u` escape starting exactly where the high one ended (`at`).
    let mut high_open = false;
    let mut at = 0;
    while let Some(off) = raw
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == b'\\'))
    {
        let esc = at + off;
        let is_u = raw.get(esc + 1) == Some(&b'u');
        let unit = if is_u {
            raw.get(esc + 2..esc + 6).and_then(hex_unit)
        } else {
            None
        };
        let high = matches!(unit, Some(0xD800..=0xDBFF));
        let low = matches!(unit, Some(0xDC00..=0xDFFF));
        if (high_open && (esc != at || !low)) || (low && !high_open) {
            return false;
        }
        high_open = high;
        at = if is_u { esc + 6 } else { esc + 2 };
    }
    !high_open
}

/// The UTF-16 code unit of four hex digits.
fn hex_unit(digits: &[u8]) -> Option<u16> {
    u16::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

/// Map-only visitor behind [`HoistEnvelope`]'s `Deserialize`.
struct EnvelopeVisitor;

impl<'de> serde::de::Visitor<'de> for EnvelopeVisitor {
    type Value = HoistEnvelope<'de>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a validation envelope object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut access: A,
    ) -> Result<Self::Value, A::Error> {
        let mut errors = None;
        while let Some(key) = access.next_key::<&'de str>()? {
            if key == "errors" {
                set_once(&mut access, &mut errors, "errors")?;
            } else {
                access.next_value::<SkippedValue>()?;
            }
        }
        let errors = errors.ok_or_else(|| serde::de::Error::missing_field("errors"))?;
        Ok(HoistEnvelope { errors })
    }
}

impl<'de> Deserialize<'de> for HoistEnvelope<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(EnvelopeVisitor)
    }
}

/// Map-only visitor behind [`HoistErrorIn`]'s `Deserialize`.
struct ErrorVisitor;

impl<'de> serde::de::Visitor<'de> for ErrorVisitor {
    type Value = HoistErrorIn<'de>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a validation error object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut access: A,
    ) -> Result<Self::Value, A::Error> {
        let (mut path, mut code, mut message): (Opt<'de>, Opt<'de>, Opt<'de>) = (None, None, None);
        while let Some(key) = access.next_key::<&'de str>()? {
            match key {
                "path" => set_once(&mut access, &mut path, "path")?,
                "code" => set_once(&mut access, &mut code, "code")?,
                "message" => set_once(&mut access, &mut message, "message")?,
                _ => {
                    access.next_value::<SkippedValue>()?;
                }
            }
        }
        let (path, code, message) = (path.flatten(), code.flatten(), message.flatten());
        Ok(HoistErrorIn {
            path,
            code,
            message,
        })
    }
}

impl<'de> Deserialize<'de> for HoistErrorIn<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ErrorVisitor)
    }
}

/// Collect hoistable `(path, code, message)` triples into wire items,
/// skipping any error that lacks a usable `path` (matches the previous
/// `e.get("path")?.as_str()?` behaviour).  Shared by the strict fast path
/// and the lenient fallback so both apply identical selection rules.
fn hoist_items<'a>(
    errors: impl Iterator<
        Item = (
            Option<Cow<'a, str>>,
            Option<Cow<'a, str>>,
            Option<Cow<'a, str>>,
        ),
    >,
) -> Vec<ValidationErrorItem<'a>> {
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

/// Lenient fallback walk over a 422 body, used by [`try_hoist_validation_errors`]
/// when the strict parse aborts.  It works on raw JSON, not a
/// `serde_json::Value` DOM, so an out-of-range number (`1e400`) in any value is
/// accepted (`RawValue` converts no number), exactly as the strict path accepts
/// it.  The selection rules are those of the `Value` walk
/// ([`hoist_from_value`]): the last occurrence of a repeated key wins, a
/// non-array `errors` or a non-object element hoists nothing, and a known field
/// hoists only as a JSON string (a wrong type reads as absent).  A lone
/// surrogate anywhere in the body rejects it, as the `Value` parse does.
///
/// `None` means the body isn't a JSON object with an `errors` array at all;
/// `Some(vec![])` means it is, but nothing in it was hoistable.  Callers apply
/// the empty→`None` collapse themselves.
fn hoist_from_raw(body_bytes: &Bytes) -> Option<Vec<ValidationErrorItem<'_>>> {
    if !surrogates_paired(body_bytes) {
        return None;
    }
    let envelope: BTreeMap<String, &RawValue> = serde_json::from_slice(body_bytes).ok()?;
    let elements: Vec<&RawValue> = serde_json::from_str(envelope.get("errors")?.get()).ok()?;
    Some(hoist_items(elements.into_iter().map(raw_error_fields)))
}

/// One raw error's hoistable `(path, code, message)`, each owned.
type RawErrorFields = (
    Option<Cow<'static, str>>,
    Option<Cow<'static, str>>,
    Option<Cow<'static, str>>,
);

/// `(path, code, message)` of one raw error.  A non-object element has none of
/// them, so it is skipped by [`hoist_items`] like a missing `path`.
fn raw_error_fields(element: &RawValue) -> RawErrorFields {
    let Ok(fields) = serde_json::from_str::<BTreeMap<String, &RawValue>>(element.get()) else {
        return (None, None, None);
    };
    let text = |key: &str| {
        fields
            .get(key)
            .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
            .map(Cow::Owned)
    };
    (text("path"), text("code"), text("message"))
}

/// Bench-only `serde_json::Value` walk over a 422 body: parse the DOM, read the
/// `errors` key, coerce it to an array, and extract `path`/`code`/`message`
/// with `as_str` (wrong types → `None`), SKIPPING any element lacking a usable
/// `path` (non-objects: `Value::get` returns `None`).  This is the DOM reference
/// that [`try_hoist_validation_errors_value_old`] measures against the typed
/// path.  It rejects an out-of-range number anywhere in the body, which
/// production's fallback ([`hoist_from_raw`]) accepts; otherwise both select the
/// same errors.
///
/// `None` means the body isn't a JSON document with an `errors` array at all;
/// `Some(vec![])` means it is, but nothing in it was hoistable.  Callers apply
/// the empty→`None` collapse themselves.
#[cfg(any(test, feature = "bench-support"))]
fn hoist_from_value<'a>(body_bytes: &Bytes) -> Option<Vec<ValidationErrorItem<'a>>> {
    let parsed: serde_json::Value = serde_json::from_slice(body_bytes).ok()?;
    let errors = parsed.get("errors")?.as_array()?;
    Some(hoist_items(errors.iter().map(|e| {
        let field = |key| {
            e.get(key)
                .and_then(serde_json::Value::as_str)
                .map(|text| Cow::Owned(text.to_owned()))
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
/// The strict path and the lenient fallback ([`hoist_from_raw`], used when a
/// known field has the wrong type or an element is not an object) reject the
/// same lone surrogates and accept the same out-of-range numbers (`1e400`) in
/// unknown values, so the hoist does not depend on an unrelated value's shape.
///
/// This is intentionally lenient — a malformed 422 body must never
/// degrade to a 5xx; the original body is still surfaced verbatim.
pub(super) fn try_hoist_validation_errors<'a>(
    headers: &http::HeaderMap,
    body_bytes: &'a Bytes,
) -> Option<Vec<ValidationErrorItem<'a>>> {
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
    let items = if let Ok(envelope) = serde_json::from_slice::<HoistEnvelope<'_>>(body_bytes) {
        hoist_items(envelope.errors.into_iter().map(|e| {
            (
                e.path.map(Cow::Borrowed),
                e.code.map(Cow::Borrowed),
                e.message.map(Cow::Borrowed),
            )
        }))
    } else {
        // The strict typed parse aborted — either a wrong-typed field
        // (`"code": 123`) OR a non-object array element (`null`, a bare
        // string). Retry through the lenient [`hoist_from_raw`] walk, which
        // keeps every still-valid error. Cold (only a hand-crafted 422 body
        // reaches here) and size-capped at `MAX_HOIST_BODY_BYTES`, so it never
        // touches the common all-string fast path.
        hoist_from_raw(body_bytes)?
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
fn try_hoist_validation_errors_value_old<'a>(
    headers: &http::HeaderMap,
    body_bytes: &Bytes,
) -> Option<Vec<ValidationErrorItem<'a>>> {
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
fn hoist_field_len_sum(items: Option<Vec<ValidationErrorItem<'_>>>) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn json_headers() -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/problem+json; charset=utf-8"),
        );
        headers
    }

    #[test]
    fn strict_and_value_hoisters_sum_the_same_optional_fields() {
        let headers = json_headers();
        let body = Bytes::from_static(
            br#"{"errors":[{"path":"name","code":"short","message":"bad"},{"path":"email"}]}"#,
        );

        assert_eq!(bench_hoist_new(&headers, &body), 17);
        assert_eq!(bench_hoist_old(&headers, &body), 17);
    }

    #[test]
    fn value_hoister_rejects_non_json_oversized_and_empty_envelopes() {
        let mut text_headers = http::HeaderMap::new();
        text_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        let valid = Bytes::from_static(br#"{"errors":[{"path":"x"}]}"#);
        assert_eq!(bench_hoist_old(&text_headers, &valid), 0);

        let oversized = Bytes::from(vec![b' '; MAX_HOIST_BODY_BYTES + 1]);
        assert_eq!(bench_hoist_old(&json_headers(), &oversized), 0);

        for body in [
            Bytes::from_static(b"not json"),
            Bytes::from_static(br"{}"),
            Bytes::from_static(br#"{"errors":null}"#),
            Bytes::from_static(br#"{"errors":[]}"#),
            Bytes::from_static(br#"{"errors":[{"message":"missing path"}]}"#),
        ] {
            assert_eq!(bench_hoist_old(&json_headers(), &body), 0);
        }
    }

    #[test]
    fn hoisting_skips_non_json_and_oversized_bodies() {
        let valid = Bytes::from_static(br#"{"errors":[{"path":"name","message":"bad"}]}"#);

        let mut text_headers = http::HeaderMap::new();
        text_headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        assert!(
            try_hoist_validation_errors(&text_headers, &valid).is_none(),
            "a non-JSON content-type must not be parsed as a validation envelope"
        );

        let oversized = Bytes::from(vec![b'x'; MAX_HOIST_BODY_BYTES + 1]);
        assert!(
            try_hoist_validation_errors(&json_headers(), &oversized).is_none(),
            "a body past the hoist cap must be surfaced verbatim, never parsed"
        );

        let hoisted = try_hoist_validation_errors(&json_headers(), &valid)
            .expect("a JSON 422 envelope within the cap must hoist");
        assert_eq!(hoisted.len(), 1);
        assert_eq!(hoisted[0].path, "name");
    }

    #[test]
    fn hoisting_rejects_content_types_shorter_than_a_json_suffix() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("json"),
        );
        let body = Bytes::from_static(br#"{"errors":[{"path":"name"}]}"#);

        assert!(try_hoist_validation_errors(&headers, &body).is_none());
    }

    /// Regression (round 11): the strict fast path must accept ONLY JSON
    /// objects.  A derived `Deserialize` also takes a JSON array through
    /// `visit_seq` (positional `[path, code, message]` / `[errors]`), so an
    /// array element or a top-level array used to hoist fields the lenient
    /// `Value` walk (and the bench twin) skip.  Both paths must now agree.
    #[rstest::rstest]
    #[case::array_element(br#"{"errors":[["email","is invalid"]]}"#.as_slice())]
    #[case::top_level_array(br#"[[{"path":"x"}]]"#.as_slice())]
    #[case::array_element_full(br#"{"errors":[["p","c","m"]]}"#.as_slice())]
    #[case::no_errors_key(br#"{"other":[{"path":"x"}]}"#.as_slice())]
    fn hoisting_never_reads_arrays_as_objects(#[case] body: &'static [u8]) {
        let body = Bytes::from_static(body);
        assert!(
            try_hoist_validation_errors(&json_headers(), &body).is_none(),
            "an array is not a validation envelope / error object"
        );
        assert_eq!(bench_hoist_old(&json_headers(), &body), 0);
    }

    /// The map-only visitors keep the derive's duplicate-key rejection (the
    /// body then retries through the `Value` walk, last key wins) and skip
    /// unknown keys; an array element next to a valid object is skipped.
    #[rstest::rstest]
    #[case::dup_errors(br#"{"errors":[{"path":"a"}],"errors":[{"path":"b"}]}"#.as_slice())]
    #[case::dup_path(br#"{"errors":[{"path":"a","path":"b"}]}"#.as_slice())]
    #[case::dup_code(br#"{"errors":[{"path":"b","code":"x","code":"y"}]}"#.as_slice())]
    #[case::dup_message(br#"{"errors":[{"path":"b","message":"x","message":"y"}]}"#.as_slice())]
    #[case::unknown_keys(br#"{"v":1,"errors":[{"extra":[1],"path":"b"}]}"#.as_slice())]
    #[case::array_next_to_object(br#"{"errors":[["a"],{"path":"b"}]}"#.as_slice())]
    fn hoisting_map_visitors_match_the_value_walk(#[case] body: &'static [u8]) {
        let body = Bytes::from_static(body);
        let items = try_hoist_validation_errors(&json_headers(), &body).expect("must hoist");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "b");
        assert_same_hoist(&body);
    }

    /// Every hoisted item as `(path, code, message)`.  The differential tests
    /// compare these, not summed lengths: two different errors of equal
    /// length would sum the same.
    fn hoisted_fields(
        items: Vec<ValidationErrorItem<'_>>,
    ) -> Vec<(String, Option<String>, Option<String>)> {
        items
            .into_iter()
            .map(|i| {
                (
                    i.path.to_string(),
                    i.code.as_deref().map(str::to_owned),
                    i.message.as_deref().map(str::to_owned),
                )
            })
            .collect()
    }

    /// The strict typed hoister and the `Value`-walk twin hoist the same
    /// items (the same fields, in the same order), or both hoist nothing.
    fn assert_same_hoist(body: &Bytes) {
        let strict = try_hoist_validation_errors(&json_headers(), body).map(hoisted_fields);
        let value =
            try_hoist_validation_errors_value_old(&json_headers(), body).map(hoisted_fields);
        assert_eq!(strict, value);
    }

    /// Regression (round 23): a lone surrogate inside a value the strict path
    /// reads as unknown used to be accepted there while the `Value` walk
    /// rejects the whole body.  Both paths must now reject it.
    #[rstest::rstest]
    #[case::lone_low_surrogate_unknown_key(br#"{"errors":[{"path":"a"}],"x":"\udc00"}"#.as_slice())]
    #[case::lone_high_surrogate_nested_unknown(br#"{"errors":[{"path":"a","extra":"\ud800"}]}"#.as_slice())]
    #[case::lone_surrogate_known_key(br#"{"errors":[{"path":"\udc00"}]}"#.as_slice())]
    fn hoisting_rejects_what_the_value_walk_rejects(#[case] body: &'static [u8]) {
        let body = Bytes::from_static(body);
        assert!(try_hoist_validation_errors(&json_headers(), &body).is_none());
        assert_eq!(bench_hoist_old(&json_headers(), &body), 0);
    }

    /// A valid surrogate pair in an ignored value is still accepted, and both
    /// paths hoist the same errors.
    #[test]
    fn hoisting_accepts_valid_surrogate_pairs_in_ignored_values() {
        // The escaped pair is assembled from pieces, so the source holds no
        // `\u` sequence the reader or a formatter could decode.
        let body = Bytes::from(
            r#"{"errors":[{"path":"a","extra":"PAIR"}],"x":"PAIR"}"#
                .replace("PAIR", concat!("\\", "ud83d", "\\", "ude00")),
        );
        let items = try_hoist_validation_errors(&json_headers(), &body).expect("valid pairs hoist");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "a");
        assert_same_hoist(&body);
    }

    /// Regression (round 24): an out-of-range number in an unknown value is
    /// accepted by the strict path (as `IgnoredAny` accepted it before round
    /// 23), so the envelope still hoists.  The fallback cases (a wrong-typed
    /// known field, which the strict parse aborts on) must accept it too, so
    /// the hoist does not depend on an unrelated value's shape (round 25).
    #[rstest::rstest]
    #[case::top_level_unknown(br#"{"errors":[{"path":"email","message":"bad"}],"debug":1e400}"#.as_slice())]
    #[case::nested_unknown(br#"{"errors":[{"path":"email","extra":[-1e400,2]}]}"#.as_slice())]
    #[case::fallback_top_level_unknown(br#"{"errors":[{"path":"email","code":123}],"debug":1e400}"#.as_slice())]
    #[case::fallback_nested_unknown(br#"{"errors":[{"path":"email","code":123,"extra":[-1e400]}]}"#.as_slice())]
    fn hoisting_accepts_out_of_range_numbers_in_unknown_values(#[case] body: &'static [u8]) {
        let body = Bytes::from_static(body);
        let items = try_hoist_validation_errors(&json_headers(), &body)
            .expect("an out-of-range number in an unknown value must not block the hoist");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "email");
    }

    /// The surrogate scan over one raw unknown value: escapes are stepped over
    /// whole, a high surrogate must be followed directly by a low one, and a
    /// lone low surrogate is rejected.
    #[rstest::rstest]
    #[case::plain_text(br#""abc""#.as_slice(), true)]
    #[case::number_out_of_range(b"1e400".as_slice(), true)]
    #[case::escaped_backslash_then_u(br#""\\ud800""#.as_slice(), true)]
    #[case::simple_escape_before_pair(br#""\n\ud83d\ude00""#.as_slice(), true)]
    #[case::valid_pair(br#""\ud83d\ude00""#.as_slice(), true)]
    #[case::lone_low(br#""\udc00""#.as_slice(), false)]
    #[case::lone_high_at_end(br#""\ud800""#.as_slice(), false)]
    #[case::high_then_plain_text(br#""\ud800x\udc00""#.as_slice(), false)]
    #[case::high_then_other_escape(br#""\ud800\n""#.as_slice(), false)]
    #[case::high_then_non_low(br#""\ud800\u0041""#.as_slice(), false)]
    #[case::two_highs_then_low(br#""\ud800\ud800\udc00""#.as_slice(), false)]
    fn surrogate_scan_matches_the_value_walk(#[case] raw: &[u8], #[case] paired: bool) {
        assert_eq!(surrogates_paired(raw), paired);
        // The `Value` walk decides the same string cases.
        if raw.starts_with(b"\"") {
            let text = std::str::from_utf8(raw).unwrap();
            assert_eq!(serde_json::from_str::<String>(text).is_ok(), paired);
        }
    }

    #[test]
    fn hoisting_collapses_an_empty_validation_envelope_to_none() {
        let body = Bytes::from_static(br#"{"errors":[]}"#);

        assert!(try_hoist_validation_errors(&json_headers(), &body).is_none());
    }
}
