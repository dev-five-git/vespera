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
        br#"{"v":1,"path":"/p"}"#,                      // missing method
        br#"{"v":1,"method":"GET"}"#,                   // missing path
        br#"{"v":1,"method":"GET","path":"/p"}x"#,      // trailing chars
        br#"{"v":1,"method":42,"path":"/p"}"#,          // method not a string
        br#"{"v":300,"method":"GET","path":"/p"}"#,     // v out of u8 range
        br#"{"v":1,"v":1,"method":"GET","path":"/p"}"#, // duplicate known field
        br#"{"v":1,"method":"GET","path":"/p","headers":{"x":1}}"#, // header value not string
        br#"{"v":1,"method":"GET","path":"/p","app":7}"#, // app not string/null
        br#"{"v":1,"method":"GET","path":"/p","headers":[]}"#, // headers not object
        // Malformed values under UNKNOWN keys must still be rejected
        // (the skip path validates the full JSON grammar, matching
        // serde_json — not the prior permissive skip that accepted them).
        br#"{"v":1,"method":"GET","path":"/p","x":"\q"}"#, // invalid string escape
        b"{\"v\":1,\"method\":\"GET\",\"path\":\"/p\",\"x\":\"\x01\"}", // unescaped control char
        br#"{"v":1,"method":"GET","path":"/p","x":tru}"#,  // truncated literal
        br#"{"v":1,"method":"GET","path":"/p","x":nul}"#,  // truncated null
        br#"{"v":1,"method":"GET","path":"/p","x":1e+}"#,  // exponent without digit
        br#"{"v":1,"method":"GET","path":"/p","x":1.}"#,   // fraction without digit
        br#"{"v":1,"method":"GET","path":"/p","x":01}"#,   // leading zero
        br#"{"v":1,"method":"GET","path":"/p","x":[}"#,    // mismatched container open
        br#"{"v":1,"method":"GET","path":"/p","x":[1,2}"#, // array closed by '}'
        br#"{"v":1,"method":"GET","path":"/p","x":{"a":1,}}"#, // trailing comma in object
        br#"{"v":01,"method":"GET","path":"/p"}"#,         // leading zero in `v`
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
    assert_eq!(
        owned(&hand),
        owned(&serde),
        "value drift on unknown-skip path"
    );
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
            assert!(
                write_wire_header_into(
                    &mut hand,
                    status,
                    &headers,
                    &metadata,
                    hand_items.as_deref(),
                ),
                "header fits u32 (status={status}, with_ve={with_ve})"
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

/// INP-01 regression: the 422 validation-error hoist is genuinely
/// best-effort — a single error object with a wrong-typed field
/// (`"code": 123`, `"message": {...}`, `"code": [..]`) must NOT abort the
/// hoist of the other valid errors.  Locks the lenient-field behaviour
/// restored after the typed-deserialize rewrite (matches the prior
/// `serde_json::Value` extract path which used `Value::as_str`).
#[test]
fn hoist_422_is_best_effort_for_wrong_typed_fields() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    // `b`/`c` carry numeric / object / array `code` & `message` — all wrong
    // types; every entry still has a usable string `path`, so the whole array
    // must hoist (wrong-typed scalars degrade to `None`, never error).
    let body = bytes::Bytes::from_static(
        br#"{"errors":[
            {"path":"a","code":"too_short","message":"min 3"},
            {"path":"b","code":123,"message":{"nested":true}},
            {"path":"c","code":[1,2],"message":null}
        ]}"#,
    );
    let items = super::hoist::try_hoist_validation_errors(&headers, &body)
        .expect("a wrong-typed field must not abort the best-effort hoist");
    assert_eq!(items.len(), 3, "every error with a path must be hoisted");
    assert_eq!(items[0].path, "a");
    assert_eq!(items[0].code.as_deref(), Some("too_short"));
    assert_eq!(items[0].message.as_deref(), Some("min 3"));
    assert_eq!(items[1].path, "b");
    assert_eq!(items[1].code, None);
    assert_eq!(items[1].message, None);
    assert_eq!(items[2].path, "c");
    assert_eq!(items[2].code, None);
    assert_eq!(items[2].message, None);
}

/// Regression: a NON-OBJECT array element (`null`, a bare string, a number)
/// must be SKIPPED, not abort the whole hoist.  Before the lenient fallback
/// switched to a `Value` walk, the typed `Vec<Struct>` retry failed to
/// deserialize the non-object element and dropped EVERY valid error with it.
#[test]
fn hoist_422_skips_non_object_array_elements() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    // A valid object, then a `null`, a bare string, and a number — the three
    // non-object elements must be skipped while the valid one still hoists.
    let body = bytes::Bytes::from_static(
        br#"{"errors":[
            {"path":"email","message":"not a valid email"},
            null,
            "oops",
            42
        ]}"#,
    );
    let items = super::hoist::try_hoist_validation_errors(&headers, &body)
        .expect("a non-object element must not discard the valid errors");
    assert_eq!(
        items.len(),
        1,
        "only the one well-formed error object should hoist"
    );
    assert_eq!(items[0].path, "email");
    assert_eq!(items[0].message.as_deref(), Some("not a valid email"));
}

/// Byte-identity for the TINY-header response fast paths in `write_headers`
/// (0 headers → `{}`; exactly 1 distinct name → no stack-array init / sort;
/// header NAME written without the escape-table scan).  The multi-header
/// `hand_serialize_matches_serde_serialize` test never reaches these
/// branches, so this locks 0 / 1-single-value / 1-repeated-value maps against
/// `serde_json` on BOTH the `Vec` and slice paths.
#[test]
fn hand_serialize_matches_serde_for_tiny_header_maps() {
    use http::{HeaderMap, HeaderName, HeaderValue};

    let empty = HeaderMap::new();

    let mut one = HeaderMap::new();
    one.insert("content-type", HeaderValue::from_static("application/json"));

    let mut one_repeated = HeaderMap::new();
    let cookie = HeaderName::from_static("set-cookie");
    one_repeated.append(cookie.clone(), HeaderValue::from_static("a=1"));
    one_repeated.append(cookie, HeaderValue::from_static("b=2; Path=/"));

    let metadata = ResponseMetadata::current();

    for (label, headers) in [
        ("0-header", &empty),
        ("1-header-single", &one),
        ("1-header-repeated", &one_repeated),
    ] {
        for status in [200u16, 204, 404] {
            let mut hand = Vec::new();
            assert!(
                write_wire_header_into(&mut hand, status, headers, &metadata, None),
                "header fits ({label}, status={status})"
            );
            let serde_view = WireResponseHeader {
                v: WIRE_VERSION,
                status,
                headers: &WireHeaders(headers),
                metadata: &metadata,
                validation_errors: None::<Vec<ValidationErrorItem>>,
            };
            let serde_bytes = serde_json::to_vec(&serde_view).expect("serde serialize");
            assert_eq!(
                &hand[4..],
                serde_bytes.as_slice(),
                "Vec-path byte drift ({label}, status={status})"
            );

            let mut hand_slice = vec![0u8; 1024];
            let n_hand = write_wire_header_into_slice(&mut hand_slice, status, headers, &metadata);
            let mut serde_slice = vec![0u8; 1024];
            let n_serde =
                write_wire_header_into_slice_serde(&mut serde_slice, status, headers, &metadata);
            assert_eq!(
                n_hand, n_serde,
                "slice length drift ({label}, status={status})"
            );
            assert_eq!(
                &hand_slice[..n_hand],
                &serde_slice[..n_serde],
                "slice-path byte drift ({label}, status={status})"
            );
        }
    }
}
