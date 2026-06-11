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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{parse_wire_header, split_wire_request};

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
        // `HeaderMap::keys` yields each distinct name exactly once.
        let mut names: Vec<&str> = self.0.keys().map(http::HeaderName::as_str).collect();
        names.sort_unstable();
        let mut map = serializer.serialize_map(Some(names.len()))?;
        for name in names {
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
/// the header view **directly into the output buffer** — no
/// intermediate `Vec` and no second memcpy of the header JSON.
///
/// Typical wire headers are well under this reservation, so the
/// serializer usually writes without reallocating.
pub const WIRE_HEADER_RESERVE: usize = 192;

fn write_wire_header_into<H: Serialize>(out: &mut Vec<u8>, view: &WireResponseHeader<'_, H>) {
    out.extend_from_slice(&[0u8; 4]);
    let start = out.len();
    serde_json::to_writer(&mut *out, view).expect("WireResponseHeader serialization is infallible");
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
    let header = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers: &WireHeaders(&headers),
        metadata: &metadata,
        validation_errors,
    };
    let mut out = Vec::with_capacity(4 + WIRE_HEADER_RESERVE + body_bytes.len());
    write_wire_header_into(&mut out, &header);
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
    let view = WireResponseHeader {
        v: WIRE_VERSION,
        status,
        headers: &WireHeaders(headers),
        metadata,
        validation_errors: None,
    };
    let mut out = Vec::with_capacity(4 + WIRE_HEADER_RESERVE);
    write_wire_header_into(&mut out, &view);
    out
}

/// Best-effort extract validation errors from a 422 JSON body.
///
/// Returns `None` (silently) for:
/// - non-JSON content-types (anything that doesn't end in `/json` or
///   `+json`)
/// - body bytes that don't parse as JSON
/// - JSON without an `errors` array, or with an empty array
///
/// This is intentionally lenient — a malformed 422 body must never
/// degrade to a 5xx; the original body is still surfaced verbatim.
fn try_hoist_validation_errors(
    headers: &http::HeaderMap,
    body_bytes: &Bytes,
) -> Option<Vec<ValidationErrorItem>> {
    // First content-type value decides (matches the previous
    // first-of-Multi behaviour).  Comparisons are case-insensitive
    // in place — no lowercased copy.
    let is_json = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            let mime = s.split(';').next().unwrap_or("").trim();
            mime.eq_ignore_ascii_case("application/json")
                || (mime.len() >= 5 && mime[mime.len() - 5..].eq_ignore_ascii_case("+json"))
        });
    if !is_json {
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

/// Deserialize the wire request header, borrowing every string from
/// `header_json` where possible (see [`WireRequestHeader`]).
pub fn parse_wire_header(header_json: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    serde_json::from_slice(header_json).map_err(|e| format!("wire header JSON parse error: {e}"))
}
