//! 422 validation-error hoisting, split out of `wire.rs` (1000-line cap):
//! the strict typed fast path plus the lenient `serde_json::Value` fallback.

use std::borrow::Cow;

use bytes::Bytes;
use serde::Deserialize;

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
/// this strict parse and is retried via the inline `serde_json::Value`
/// fallback walk in [`try_hoist_validation_errors`], so the hoist stays
/// genuinely best-effort without taxing the common case.
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

impl<'de> Deserialize<'de> for HoistEnvelope<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
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
                        access.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                let errors = errors.ok_or_else(|| serde::de::Error::missing_field("errors"))?;
                Ok(HoistEnvelope { errors })
            }
        }
        deserializer.deserialize_map(V)
    }
}

impl<'de> Deserialize<'de> for HoistErrorIn<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = HoistErrorIn<'de>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a validation error object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let (mut path, mut code, mut message): (Opt<'de>, Opt<'de>, Opt<'de>) =
                    (None, None, None);
                while let Some(key) = access.next_key::<&'de str>()? {
                    match key {
                        "path" => set_once(&mut access, &mut path, "path")?,
                        "code" => set_once(&mut access, &mut code, "code")?,
                        "message" => set_once(&mut access, &mut message, "message")?,
                        _ => {
                            access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(HoistErrorIn {
                    path: path.flatten(),
                    code: code.flatten(),
                    message: message.flatten(),
                })
            }
        }
        deserializer.deserialize_map(V)
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
        assert_eq!(
            bench_hoist_new(&json_headers(), &body),
            bench_hoist_old(&json_headers(), &body)
        );
    }

    #[test]
    fn hoisting_collapses_an_empty_validation_envelope_to_none() {
        let body = Bytes::from_static(br#"{"errors":[]}"#);

        assert!(try_hoist_validation_errors(&json_headers(), &body).is_none());
    }
}
