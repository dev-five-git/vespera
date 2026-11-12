//! Latin-1 mapping for HTTP header **values** crossing the wire header JSON.
//!
//! HTTP header values are byte strings (RFC 9110 `obs-text` allows
//! `0x80..=0xFF`), but the wire header is UTF-8 JSON.  Servlet containers
//! decode request header bytes as ISO-8859-1 (one byte → one `char`
//! `U+0000..=U+00FF`) and encode response header `char`s back the same way,
//! so the lossless contract is a byte ↔ `char` mapping:
//!
//! - **Response** ([`header_value_latin1`]): every value byte becomes the
//!   `char` with the same code point.  A value that is pure visible ASCII is
//!   borrowed unchanged, so ASCII output stays byte-identical; a value with
//!   `obs-text` bytes (previously rendered as `""`) now round-trips exactly
//!   through the Java `addHeader` Latin-1 encoder.
//! - **Request** ([`header_value_wire_bytes`]): a value whose `char`s are all
//!   `<= U+00FF` maps each `char` back to one byte (the servlet's Latin-1
//!   view of the raw request bytes).  A value containing any `char` above
//!   `U+00FF` cannot be a Latin-1 view, so it keeps its UTF-8 encoding —
//!   backwards compatible with wire producers that send UTF-8 text.
//!
//! Header **names** are ASCII tokens and are never remapped.

use std::borrow::Cow;

/// Render a response header value as its Latin-1 `str` view: visible ASCII
/// is borrowed as-is, any other value maps each byte to `char::from(byte)`.
#[must_use]
pub fn header_value_latin1(value: &http::HeaderValue) -> Cow<'_, str> {
    value.to_str().map_or_else(
        |_| Cow::Owned(value.as_bytes().iter().copied().map(char::from).collect()),
        Cow::Borrowed,
    )
}

/// Bytes a request header value string stands for under the wire contract:
/// ASCII and any string whose `char`s all fit in `U+0000..=U+00FF` map one
/// `char` → one byte; a string with a wider `char` keeps its UTF-8 bytes.
#[must_use]
pub fn header_value_wire_bytes(value: &str) -> Cow<'_, [u8]> {
    if value.is_ascii() {
        return Cow::Borrowed(value.as_bytes());
    }
    value
        .chars()
        .map(u8::try_from)
        .collect::<Result<Vec<u8>, _>>()
        .map_or(Cow::Borrowed(value.as_bytes()), Cow::Owned)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::ascii(b"attachment; filename=\"a.pdf\"", "attachment; filename=\"a.pdf\"")]
    #[case::utf8_bytes(
        "attachment; filename=\"résumé.pdf\"".as_bytes(),
        "attachment; filename=\"rÃ©sumÃ©.pdf\""
    )]
    #[case::latin1_bytes(b"caf\xe9", "caf\u{e9}")]
    #[case::invalid_utf8(b"\xff\xfe", "\u{ff}\u{fe}")]
    fn response_values_map_each_byte_to_one_char(#[case] raw: &[u8], #[case] expected: &str) {
        let value = http::HeaderValue::from_bytes(raw).unwrap();
        let rendered = header_value_latin1(&value);
        assert_eq!(rendered, expected);
        // Latin-1 re-encoding of the rendered chars restores the exact bytes.
        assert_eq!(header_value_wire_bytes(&rendered), raw);
    }

    #[test]
    fn ascii_response_value_is_borrowed() {
        let value = http::HeaderValue::from_static("text/plain");
        assert!(matches!(
            header_value_latin1(&value),
            Cow::Borrowed("text/plain")
        ));
    }

    #[rstest]
    #[case::ascii("plain", b"plain".as_slice())]
    #[case::latin1_view_of_utf8("rÃ©sumÃ©", "résumé".as_bytes())]
    #[case::latin1_e_acute("\u{e9}", b"\xe9".as_slice())]
    #[case::wide_char_keeps_utf8("한글", "한글".as_bytes())]
    #[case::mixed_wide_keeps_utf8("é한", "é한".as_bytes())]
    fn request_values_map_latin1_chars_to_bytes(#[case] value: &str, #[case] expected: &[u8]) {
        assert_eq!(header_value_wire_bytes(value), expected);
    }

    #[test]
    fn ascii_and_wide_request_values_are_borrowed() {
        assert!(matches!(header_value_wire_bytes("abc"), Cow::Borrowed(_)));
        assert!(matches!(header_value_wire_bytes("한"), Cow::Borrowed(_)));
        assert!(matches!(header_value_wire_bytes("é"), Cow::Owned(_)));
    }
}
