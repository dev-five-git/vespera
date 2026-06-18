//! Binary wire format: request-header borrowing deserialization,
//! response-header serialization (straight from `http::HeaderMap`),
//! frame split/parse, and 422 `validation_errors` hoisting.
//!
//! The serialized byte layout is **locked** by tests/wire_contract.rs.

use std::borrow::Cow;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

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

use header_write::JsonSink;

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use crate::envelope::ResponseMetadata;

    use super::{
        ValidationErrorItem, WIRE_VERSION, WireHeaders, WireRequestHeader, WireResponseHeader,
        parse_wire_header, parse_wire_header_serde, split_wire_request, write_wire_header_into,
        write_wire_header_into_slice, write_wire_header_into_slice_serde,
    };

    /// Pins the zero-copy contract: the returned body must point into
    /// the original input allocation (no memcpy of the tail).
    #[test]
    fn split_wire_request_body_is_zero_copy() {
        let header = br#"{"v":1,"method":"POST","path":"/x"}"#;
        let body = vec![0xABu8; 1024];
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(header);
        wire.extend_from_slice(&body);

        let input_ptr = wire.as_ptr() as usize;
        let body_offset = 4 + header.len();
        let (_, parsed_body) = split_wire_request(wire).expect("valid wire request");

        assert_eq!(parsed_body.len(), 1024);
        assert_eq!(
            parsed_body.as_ptr() as usize,
            input_ptr + body_offset,
            "body must alias the original input buffer (zero-copy)"
        );
    }

    /// Pins the borrowed-deserialization contract: header strings
    /// without JSON escapes must borrow straight from the wire bytes
    /// (no per-string allocation), with `Cow::Owned` reserved for
    /// escaped values.
    #[test]
    fn parse_wire_header_borrows_plain_strings() {
        let header_json =
                br#"{"v":1,"method":"POST","path":"/users","query":"a=1","headers":{"x-a":"plain","x-b":"esc\"aped"},"app":"admin"}"#;
        let header = parse_wire_header(header_json).expect("valid header");

        let header_value = |name: &str| {
            header
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v)
        };

        assert!(matches!(header.method, Cow::Borrowed("POST")));
        assert!(matches!(header.path, Cow::Borrowed("/users")));
        assert!(matches!(header.query, Cow::Borrowed("a=1")));
        assert!(matches!(header.app.as_ref(), Some(Cow::Borrowed("admin"))));
        assert!(matches!(header_value("x-a"), Some(Cow::Borrowed("plain"))));
        // Escaped value falls back to owned — correctness over borrow.
        assert_eq!(
            header_value("x-b").map(std::convert::AsRef::as_ref),
            Some("esc\"aped")
        );
    }

    // ── hand-rolled vs serde_json round-trip (value / byte identity) ──

    /// Owned, comparable projection of a parsed header — the borrow vs
    /// owned `Cow` distinction does not affect VALUE equality.
    type OwnedHeader = (
        u8,
        String,
        String,
        String,
        Option<String>,
        Vec<(String, String)>,
    );

    fn owned(h: &WireRequestHeader<'_>) -> OwnedHeader {
        (
            h.v,
            h.method.to_string(),
            h.path.to_string(),
            h.query.to_string(),
            h.app.as_ref().map(ToString::to_string),
            h.headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    /// The hand-rolled request parser must produce the SAME values as
    /// `serde_json` across arbitrary key order, ignored unknown keys,
    /// escapes (quote / backslash / control), `\uXXXX` + surrogate pairs,
    /// non-ASCII UTF-8, escaped keys, duplicate header names, and
    /// string-or-null `app`.
    #[test]
    fn hand_parse_matches_serde_parse() {
        let cases: &[&[u8]] = &[
            br#"{"v":1,"method":"GET","path":"/health"}"#,
            // arbitrary key order + query
            br#"{"method":"POST","path":"/users","v":1,"query":"a=1&b=2"}"#,
            // escaped values: quote, backslash, newline, tab
            br#"{"v":1,"method":"GET","path":"/p","headers":{"x-q":"he said \"hi\"","x-bs":"a\\b","x-nl":"l1\nl2\ttab"}}"#,
            // escaped key (\u0065 == 'e') -> owned key
            br#"{"v":1,"method":"GET","path":"/p","headers":{"x-\u0065sc":"v"}}"#,
            // non-ASCII / UTF-8 (borrowed) path + emoji value
            "{\"v\":1,\"method\":\"GET\",\"path\":\"/café\",\"headers\":{\"x-emoji\":\"😀\"}}".as_bytes(),
            // \uXXXX BMP + UTF-16 surrogate pair
            br#"{"v":1,"method":"GET","path":"/p","headers":{"x-smile":"\uD83D\uDE00","x-e":"\u00e9"}}"#,
            // app: null and app: trimmed string
            br#"{"v":1,"method":"GET","path":"/p","app":null}"#,
            br#"{"v":1,"method":"GET","path":"/p","app":"  admin  "}"#,
            // unknown fields (object / array / number / bool / null) ignored
            br#"{"v":1,"method":"GET","path":"/p","extra":{"nested":[1,2,3]},"flag":true,"n":42,"z":null}"#,
            // empty headers object + duplicate header NAMES preserved
            br#"{"v":1,"method":"GET","path":"/p","headers":{}}"#,
            br#"{"v":1,"method":"GET","path":"/p","headers":{"x-a":"1","x-a":"2"}}"#,
            // VALID but complex values under an UNKNOWN key — the strict
            // skip must still ACCEPT every JSON-legal form (negative /
            // float / exponent numbers, escaped strings, nested arrays and
            // objects, the three literals) so forward-compat fields aren't
            // over-rejected.
            br#"{"v":1,"method":"GET","path":"/p","a":-3.14e10,"b":"esc\"d\n","c":[true,null,{"x":1}],"d":0}"#,
        ];
        for case in cases {
            match (parse_wire_header(case), parse_wire_header_serde(case)) {
                (Ok(hand), Ok(serde)) => assert_eq!(
                    owned(&hand),
                    owned(&serde),
                    "value drift on {}",
                    String::from_utf8_lossy(case)
                ),
                (Err(_), Err(_)) => {}
                (hand, serde) => panic!(
                    "accept/reject divergence on {}: hand_ok={} serde_ok={}",
                    String::from_utf8_lossy(case),
                    hand.is_ok(),
                    serde.is_ok()
                ),
            }
        }
    }

    /// Malformed inputs the serde derive rejects must also be rejected by
    /// the hand-rolled parser (and never panic).
    #[test]
    fn hand_parse_rejects_what_serde_rejects() {
        let bad: &[&[u8]] = &[
            b"not json",
            br#"{"v":1,"path":"/p"}"#,                  // missing method
            br#"{"v":1,"method":"GET"}"#,               // missing path
            br#"{"v":1,"method":"GET","path":"/p"}x"#,  // trailing chars
            br#"{"v":1,"method":42,"path":"/p"}"#,      // method not a string
            br#"{"v":300,"method":"GET","path":"/p"}"#, // v out of u8 range
            br#"{"v":1,"v":1,"method":"GET","path":"/p"}"#, // duplicate known field
            br#"{"v":1,"method":"GET","path":"/p","headers":{"x":1}}"#, // header value not string
            br#"{"v":1,"method":"GET","path":"/p","app":7}"#, // app not string/null
            br#"{"v":1,"method":"GET","path":"/p","headers":[]}"#, // headers not object
            // Malformed values under UNKNOWN keys must still be rejected
            // (the skip path validates the full JSON grammar, matching
            // serde_json — not the prior permissive skip that accepted them).
            br#"{"v":1,"method":"GET","path":"/p","x":"\q"}"#, // invalid string escape
            b"{\"v\":1,\"method\":\"GET\",\"path\":\"/p\",\"x\":\"\x01\"}", // unescaped control char
            br#"{"v":1,"method":"GET","path":"/p","x":tru}"#,               // truncated literal
            br#"{"v":1,"method":"GET","path":"/p","x":nul}"#,               // truncated null
            br#"{"v":1,"method":"GET","path":"/p","x":1e+}"#, // exponent without digit
            br#"{"v":1,"method":"GET","path":"/p","x":1.}"#,  // fraction without digit
            br#"{"v":1,"method":"GET","path":"/p","x":01}"#,  // leading zero
            br#"{"v":1,"method":"GET","path":"/p","x":[}"#,   // mismatched container open
            br#"{"v":1,"method":"GET","path":"/p","x":[1,2}"#, // array closed by '}'
            br#"{"v":1,"method":"GET","path":"/p","x":{"a":1,}}"#, // trailing comma in object
            br#"{"v":01,"method":"GET","path":"/p"}"#,        // leading zero in `v`
        ];
        for case in bad {
            assert!(
                parse_wire_header(case).is_err(),
                "hand parser must reject {}",
                String::from_utf8_lossy(case)
            );
            assert!(
                parse_wire_header_serde(case).is_err(),
                "serde parser must reject {}",
                String::from_utf8_lossy(case)
            );
        }
    }

    /// A very deeply nested unknown-field value must be walked by the
    /// ITERATIVE skip (no native recursion) so it can never overflow the
    /// stack and crash the host JVM across the JNI boundary — and it must
    /// stay accept/reject-identical to `serde_json`, whose `ignore_value` is
    /// likewise iterative and imposes NO recursion cap on ignored values
    /// (so a well-formed deep value is *accepted*, not rejected).  The test
    /// completing at all proves neither path blew the stack.
    #[test]
    fn hand_parse_handles_deep_unknown_nesting_without_overflow() {
        // Depth far beyond any native recursion limit (a recursive skip would
        // overflow the stack here).
        let depth = 50_000usize;

        // Well-formed deep nesting under an unknown key: both ACCEPT (serde's
        // iterative ignore imposes no cap), value-identical (no fields stored).
        let mut ok = br#"{"v":1,"method":"GET","path":"/p","z":"#.to_vec();
        ok.extend(std::iter::repeat_n(b'[', depth));
        ok.extend(std::iter::repeat_n(b']', depth));
        ok.push(b'}');
        assert_eq!(
            parse_wire_header(&ok).is_ok(),
            parse_wire_header_serde(&ok).is_ok(),
            "hand vs serde accept/reject must match on deep well-formed nesting"
        );
        assert!(
            parse_wire_header(&ok).is_ok(),
            "well-formed deep unknown nesting must be accepted (matches serde)"
        );

        // Deep UNCLOSED nesting: both REJECT (grammar error), still no overflow.
        let mut bad = br#"{"v":1,"method":"GET","path":"/p","z":"#.to_vec();
        bad.extend(std::iter::repeat_n(b'[', depth)); // never closed
        assert!(parse_wire_header(&bad).is_err());
        assert!(parse_wire_header_serde(&bad).is_err());
    }

    /// A shallow unknown-field value (well within the depth cap) carrying
    /// escaped strings, a `\uXXXX` BMP escape, a UTF-16 surrogate pair, and a
    /// nested array must still PARSE via the non-allocating skip path, with
    /// the known fields intact and value-identical to serde — locking the
    /// `skip_string` / `validate_*` twins against the decoding `read_string`.
    #[test]
    fn hand_parse_accepts_shallow_unknown_with_escapes() {
        let json = br#"{"v":1,"method":"GET","path":"/p","x-meta":{"trace":"a\"b\nc\td","u":"\u00e9\uD83D\uDE00"},"flags":[true,null,42,-3.14e2]}"#;
        let hand = parse_wire_header(json).expect("hand accepts forward-compat unknown fields");
        let serde = parse_wire_header_serde(json).expect("serde accepts the same input");
        assert_eq!(owned(&hand), owned(&serde), "value drift on unknown-skip path");
        assert_eq!(hand.method.as_ref(), "GET");
        assert_eq!(hand.path.as_ref(), "/p");
    }

    /// Fresh `validation_errors` table exercising the full escape set
    /// (quote, backslash, newline, a `\u0001` control, tab, non-ASCII)
    /// plus the skip-if-none `code`/`message` fields.
    fn validation_items() -> Vec<ValidationErrorItem> {
        vec![
            ValidationErrorItem {
                path: "user\"name".to_owned(),
                code: Some("E\\01".to_owned()),
                message: Some("bad\nvalue\u{1}\tré".to_owned()),
            },
            ValidationErrorItem {
                path: "tags".to_owned(),
                code: None,
                message: None,
            },
        ]
    }

    /// The hand-rolled response serializer must produce BYTE-IDENTICAL
    /// output to `serde_json` across statuses, the optional
    /// `validation_errors` array, sorted single/multi headers, non-UTF-8
    /// values (rendered `""`), and the full string escape set — proven by
    /// both the `Vec` path and the `&mut [u8]` slice path.
    #[test]
    fn hand_serialize_matches_serde_serialize() {
        use http::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert("content-length", HeaderValue::from_static("42"));
        headers.insert("x-quote", HeaderValue::from_bytes(b"a\"b").unwrap());
        headers.insert("x-backslash", HeaderValue::from_bytes(b"a\\b").unwrap());
        // Valid UTF-8 obs-text passes through verbatim (no `/` escaping).
        headers.insert(
            "x-utf8",
            HeaderValue::from_bytes("ré sumé/path".as_bytes()).unwrap(),
        );
        // Invalid UTF-8 value -> rendered as "" by both paths.
        headers.insert("x-binary", HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap());
        let cookie = HeaderName::from_static("set-cookie");
        headers.append(cookie.clone(), HeaderValue::from_static("a=1"));
        headers.append(cookie.clone(), HeaderValue::from_static("b=2; Path=/"));
        headers.append(cookie, HeaderValue::from_bytes(b"c=\"q\"").unwrap());

        let metadata = ResponseMetadata::current();

        for status in [200u16, 404, 422] {
            for with_ve in [false, true] {
                let hand_items = with_ve.then(validation_items);
                let mut hand = Vec::new();
                write_wire_header_into(
                    &mut hand,
                    status,
                    &headers,
                    &metadata,
                    hand_items.as_deref(),
                );

                let serde_view = WireResponseHeader {
                    v: WIRE_VERSION,
                    status,
                    headers: &WireHeaders(&headers),
                    metadata: &metadata,
                    validation_errors: with_ve.then(validation_items),
                };
                let serde_bytes = serde_json::to_vec(&serde_view).expect("serde serialize");

                assert_eq!(
                    &hand[4..],
                    serde_bytes.as_slice(),
                    "Vec-path byte drift (status={status}, with_ve={with_ve})"
                );
                // Length prefix must equal the JSON byte length.
                assert_eq!(
                    u32::from_be_bytes(hand[..4].try_into().unwrap()) as usize,
                    serde_bytes.len()
                );
            }

            // Slice path (always None validation_errors): hand vs serde.
            let mut hand_slice = vec![0u8; 4096];
            let n_hand = write_wire_header_into_slice(&mut hand_slice, status, &headers, &metadata);
            let mut serde_slice = vec![0u8; 4096];
            let n_serde =
                write_wire_header_into_slice_serde(&mut serde_slice, status, &headers, &metadata);
            assert_eq!(n_hand, n_serde, "slice length drift (status={status})");
            assert_eq!(
                &hand_slice[..n_hand],
                &serde_slice[..n_serde],
                "slice-path byte drift (status={status})"
            );
        }
    }
}

/// Wire format protocol version.  The JSON header's `v` field MUST
/// equal this for requests; responses always emit this value.
pub const WIRE_VERSION: u8 = 1;

// ── Wire Format Types (internal) ─────────────────────────────────────

/// Request wire header, deserialized **borrowing from the input
/// buffer**: every string field is a `Cow` that points straight into
/// the wire bytes (zero allocation) unless the JSON value contains
/// escape sequences, in which case deserialization transparently
/// falls back to an owned copy.
///
/// Direct `Cow<str>` fields borrow via serde-derive's `borrow`
/// special-casing; `headers` and `app` need the custom
/// [`de_cow_map`] / [`de_opt_cow`] deserializers because serde's
/// stock `Cow` impl inside containers always copies.
#[derive(Debug, Deserialize)]
pub struct WireRequestHeader<'a> {
    /// Wire protocol version; clients MUST send 1.
    #[serde(default)]
    pub v: u8,
    #[serde(borrow)]
    pub method: Cow<'a, str>,
    #[serde(borrow)]
    pub path: Cow<'a, str>,
    #[serde(default, borrow)]
    pub query: Cow<'a, str>,
    /// Request headers as a flat list — dispatch only ever *iterates*
    /// them (never looks one up by key), so a `Vec` skips the
    /// `HashMap` bucket allocation + per-key hashing entirely.
    /// Repeated names are forwarded as repeated request headers
    /// (valid HTTP; the previous `HashMap` silently kept the last
    /// duplicate of a degenerate duplicate-key JSON header).
    #[serde(default, borrow, deserialize_with = "de_cow_pairs")]
    pub headers: CowPairs<'a>,
    /// Optional name of the target app for multi-app routing.  When
    /// omitted (or empty), the request is dispatched to the default
    /// app registered via [`register_app`].  Use [`register_app_named`]
    /// to register additional named apps.
    #[serde(default, borrow, deserialize_with = "de_opt_cow")]
    pub app: Option<Cow<'a, str>>,
}

/// `Cow<str>` wrapper whose `Deserialize` impl borrows from the input
/// when the JSON string carries no escape sequences.
struct BorrowableCow<'a>(Cow<'a, str>);

impl<'de> Deserialize<'de> for BorrowableCow<'de> {
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
/// possible.
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
            BorrowableCow::deserialize(deserializer).map(|c| Some(c.0))
        }
    }
    deserializer.deserialize_option(V)
}

// wire-order locked — field order defines the serialized wire header
// byte layout (`v`, `status`, `headers`, `metadata`,
// `validation_errors?`).  See tests/wire_contract.rs.
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
struct WireHeaders<'a>(&'a http::HeaderMap);

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

fn write_wire_header_into(
    out: &mut Vec<u8>,
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
    validation_errors: Option<&[ValidationErrorItem]>,
) {
    out.extend_from_slice(&[0u8; 4]);
    let start = out.len();
    header_write::write_response_header(out, status, headers, metadata, validation_errors);
    let header_len =
        u32::try_from(out.len() - start).expect("response header JSON exceeds u32::MAX bytes");
    out[start - 4..start].copy_from_slice(&header_len.to_be_bytes());
}

/// One entry in the wire header's `validation_errors` array.  Fields
/// are best-effort: missing values in the source body become `None`.
#[derive(Debug, Serialize)]
struct ValidationErrorItem {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    let parts = (
        status,
        headers,
        Bytes::copy_from_slice(msg.as_bytes()),
        metadata,
    );
    to_wire_bytes(parts)
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
        try_hoist_validation_errors(&headers, &body_bytes)
    } else {
        None
    };
    let mut out = Vec::with_capacity(4 + WIRE_HEADER_RESERVE + body_bytes.len());
    write_wire_header_into(
        &mut out,
        status,
        &headers,
        &metadata,
        validation_errors.as_deref(),
    );
    out.extend_from_slice(&body_bytes);
    out
}

/// Build wire-format header bytes (`[u32 BE header_len | JSON header]`)
/// without a body — used by the `*_with_header` callback variants.
pub fn build_wire_header_bytes(
    status: u16,
    headers: &http::HeaderMap,
    metadata: &ResponseMetadata,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + WIRE_HEADER_RESERVE);
    write_wire_header_into(&mut out, status, headers, metadata, None);
    out
}

/// `io::Write` adapter over a fixed `&mut [u8]`: copies the prefix that
/// fits and *counts* the rest, so a serializer can fill the caller's
/// buffer and still report the exact size it needed on overflow —
/// without allocating or panicking.  `pos` is the running total of bytes
/// the writer was asked to write (it may exceed `buf.len()`).
struct SliceWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

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
    if header_total <= out.len() {
        let json_len =
            u32::try_from(header_total - 4).expect("response header JSON exceeds u32::MAX bytes");
        out[0..4].copy_from_slice(&json_len.to_be_bytes());
    }
    header_total
}

/// `serde_json`-backed twin of [`write_wire_header_into_slice`], retained
/// **only** as the "before" arm of the criterion A/B in
/// `benches/dispatch.rs` (via [`crate::bench_support`]) so hand-rolled vs
/// `serde_json` are measured in the same run.  Not part of the public
/// API and not used on any production path.
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
            let mime = s.split(';').next().unwrap_or("").trim();
            mime.eq_ignore_ascii_case("application/json")
                || (mime.len() >= 5 && mime[mime.len() - 5..].eq_ignore_ascii_case("+json"))
        })
}

/// Typed shape of the validation envelope, deserialized **directly** from the
/// 422 body — skips building the intermediate `serde_json::Value` DOM (the
/// object map + array vec + per-error maps + interned string keys) the
/// previous reparse allocated, going straight to the `Vec<HoistErrorIn>` whose
/// owned strings [`ValidationErrorItem`] needs anyway.  Unknown fields are
/// ignored and every field is optional, so an odd error object never aborts
/// the parse for a framework-generated (all-string-field) envelope.
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
fn try_hoist_validation_errors(
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
    // Direct typed deserialize — no intermediate `serde_json::Value` DOM.
    let envelope: HoistEnvelope = serde_json::from_slice(body_bytes).ok()?;
    let items: Vec<ValidationErrorItem> = envelope
        .errors
        .into_iter()
        .filter_map(|e| {
            // Match the previous behaviour: an error with no `path` is
            // skipped while the rest are still hoisted.
            Some(ValidationErrorItem {
                path: e.path?,
                code: e.code,
                message: e.message,
            })
        })
        .collect();
    if items.is_empty() { None } else { Some(items) }
}

/// **Bench-only** `serde_json::Value` twin of [`try_hoist_validation_errors`],
/// retained as the "before" arm of the `hoist_422_ab` criterion A/B
/// (same-run, noise-robust — mirroring the `wire_header_serde` /
/// `request_build_ab` twins).  Parses the body into a full `Value` DOM then
/// re-extracts each field — the allocation-heavier path the typed deserialize
/// replaced; byte-identical result for the framework-generated envelope.  Not
/// used on any production path.
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

/// Split a wire-format request into its header-JSON region and body —
/// both true zero-copy O(1) refcount views of the input allocation
/// (unlike `Vec::split_off`, which allocates a new vector and memcpys
/// the tail).
///
/// Two-phase with [`parse_wire_header`] so the deserialized header
/// can **borrow** its strings from the returned header bytes (the
/// caller keeps them alive on its stack frame).
pub fn split_wire_request(input: Vec<u8>) -> Result<(Bytes, Bytes), String> {
    if input.len() < 4 {
        return Err(format!(
            "wire input too short: {} bytes, need at least 4",
            input.len()
        ));
    }
    let mut input = Bytes::from(input);
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

/// Sum every hoisted item's field byte lengths so neither `hoist_422_ab` arm
/// can be optimised down to a partial parse.  `None` (no hoist) sums to 0.
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
#[doc(hidden)]
#[must_use]
pub fn bench_hoist_new(headers: &http::HeaderMap, body: &Bytes) -> usize {
    hoist_field_len_sum(try_hoist_validation_errors(headers, body))
}

/// Bench A/B: previous `serde_json::Value` DOM 422 validation hoist cost.
/// Bench-only.
#[doc(hidden)]
#[must_use]
pub fn bench_hoist_old(headers: &http::HeaderMap, body: &Bytes) -> usize {
    hoist_field_len_sum(try_hoist_validation_errors_value_old(headers, body))
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
