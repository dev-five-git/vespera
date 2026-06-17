//! Hand-rolled deserializer for the **fixed-schema request wire header**
//! — the byte-for-byte replacement for the `serde_json::from_slice`
//! path that used to drive [`super::WireRequestHeader`]'s derive.
//!
//! Behaviour matches `serde_json` + the serde derive on
//! [`super::WireRequestHeader`] (locked by the in-crate round-trip
//! property test in `wire.rs` and the fuzz harness in
//! `tests/wire_robustness.rs`):
//!
//! * accepts the object in **any key order** and **ignores unknown
//!   keys** (forward-compat);
//! * every string **borrows** straight from the input
//!   ([`Cow::Borrowed`]) when it carries no escapes, and falls back to
//!   an owned decode ([`Cow::Owned`]) for `\" \\ \/ \b \f \n \r \t
//!   \uXXXX` — including UTF-16 surrogate pairs;
//! * `v` defaults to `0`, `query` to empty, `headers` to empty, `app`
//!   to `None`; `method`/`path` are required;
//! * duplicate known keys, lone/invalid surrogates, bad escapes,
//!   unescaped control characters, invalid UTF-8, and trailing content
//!   are **parse errors** (never a panic).

use std::borrow::Cow;

use super::{CowPairs, WireRequestHeader};

/// Parse the request wire header, borrowing every plain string straight
/// from `input`.  Returns a bare error message; the caller
/// ([`super::parse_wire_header`]) adds the `wire header JSON parse
/// error:` prefix to match the previous `serde_json` shape.
pub(super) fn parse(input: &[u8]) -> Result<WireRequestHeader<'_>, String> {
    let mut parser = Parser { input, pos: 0 };
    let header = parser.parse_header()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return Err("trailing characters after wire header object".to_owned());
    }
    Ok(header)
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_header(&mut self) -> Result<WireRequestHeader<'a>, String> {
        self.expect(b'{')?;

        let mut v_val: u8 = 0;
        let mut v_seen = false;
        let mut method: Option<Cow<'a, str>> = None;
        let mut path: Option<Cow<'a, str>> = None;
        let mut query: Cow<'a, str> = Cow::Borrowed("");
        let mut query_seen = false;
        let mut headers: CowPairs<'a> = Vec::new();
        let mut headers_seen = false;
        let mut app: Option<Cow<'a, str>> = None;
        let mut app_seen = false;

        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            // Empty object: method/path missing -> reported below.
        } else {
            loop {
                let key = self.read_string()?;
                self.expect(b':')?;
                // Match known fields by content; serde rejects a duplicate
                // of ANY known field (even the `#[serde(default)]` ones)
                // while skipping unknown keys' values.
                match key.as_ref() {
                    "v" => {
                        if v_seen {
                            return Err("duplicate field `v`".to_owned());
                        }
                        v_val = self.read_u8()?;
                        v_seen = true;
                    }
                    "method" => {
                        if method.is_some() {
                            return Err("duplicate field `method`".to_owned());
                        }
                        method = Some(self.read_string()?);
                    }
                    "path" => {
                        if path.is_some() {
                            return Err("duplicate field `path`".to_owned());
                        }
                        path = Some(self.read_string()?);
                    }
                    "query" => {
                        if query_seen {
                            return Err("duplicate field `query`".to_owned());
                        }
                        query = self.read_string()?;
                        query_seen = true;
                    }
                    "headers" => {
                        if headers_seen {
                            return Err("duplicate field `headers`".to_owned());
                        }
                        headers = self.read_headers()?;
                        headers_seen = true;
                    }
                    "app" => {
                        if app_seen {
                            return Err("duplicate field `app`".to_owned());
                        }
                        app = self.read_opt_string()?;
                        app_seen = true;
                    }
                    _ => self.skip_value()?,
                }
                self.skip_ws();
                match self.cur() {
                    Some(b',') => {
                        self.pos += 1;
                        self.skip_ws();
                    }
                    Some(b'}') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err("expected ',' or '}' in wire header object".to_owned()),
                }
            }
        }

        let method = method.ok_or_else(|| "missing field `method`".to_owned())?;
        let path = path.ok_or_else(|| "missing field `path`".to_owned())?;
        Ok(WireRequestHeader {
            v: v_val,
            method,
            path,
            query,
            headers,
            app,
        })
    }

    /// Parse a JSON object of `string -> string` into a flat `Vec` of
    /// `(name, value)` pairs, each borrowing from the input where
    /// possible.  Repeated names are preserved (matching the previous
    /// `de_cow_pairs` `Vec` behaviour — no dedup).
    fn read_headers(&mut self) -> Result<CowPairs<'a>, String> {
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            // Zero-allocation fast path for the common bodyless /
            // headerless request — no capacity is reserved for `{}`.
            self.pos += 1;
            return Ok(Vec::new());
        }
        // Pre-reserve for a typical request's header count so the first
        // few pushes don't trigger the Vec's early doubling reallocations
        // (the previous `Vec::new()` reallocated at 1, 2, 4, 8, ...).
        let mut out: CowPairs<'a> = Vec::with_capacity(8);
        loop {
            let name = self.read_string()?;
            self.expect(b':')?;
            let value = self.read_string()?;
            out.push((name, value));
            self.skip_ws();
            match self.cur() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err("expected ',' or '}' in headers object".to_owned()),
            }
        }
        Ok(out)
    }

    /// Parse `app`: a JSON string (borrow/owned) or `null` (`None`),
    /// matching serde's `deserialize_option` -> string-or-null contract.
    fn read_opt_string(&mut self) -> Result<Option<Cow<'a, str>>, String> {
        self.skip_ws();
        if self.cur() == Some(b'n') {
            self.expect_literal(b"null")?;
            Ok(None)
        } else {
            Ok(Some(self.read_string()?))
        }
    }

    /// Read a JSON string starting at the current position, returning a
    /// borrowed slice when the value has no escapes and an owned decode
    /// otherwise.  Errors on unterminated strings, unescaped control
    /// characters, and invalid UTF-8 (all of which `serde_json` rejects).
    fn read_string(&mut self) -> Result<Cow<'a, str>, String> {
        self.skip_ws();
        if self.cur() != Some(b'"') {
            return Err("expected string".to_owned());
        }
        self.pos += 1;
        // Copy the `&'a [u8]` reference out of `self` so the borrowed
        // slice carries lifetime `'a` (tied to the input data), not the
        // shorter `&mut self` borrow.
        let input = self.input;
        let start = self.pos;
        loop {
            match input.get(self.pos) {
                None => return Err("unterminated string".to_owned()),
                Some(&b'"') => {
                    let slice = &input[start..self.pos];
                    self.pos += 1;
                    let s = std::str::from_utf8(slice)
                        .map_err(|_| "invalid UTF-8 in string".to_owned())?;
                    return Ok(Cow::Borrowed(s));
                }
                Some(&b'\\') => return self.read_string_escaped(start),
                Some(&b) if b < 0x20 => {
                    return Err("control character in string".to_owned());
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Owned-decode tail of [`Self::read_string`]: copies the already
    /// scanned plain prefix `[start, pos)`, then decodes escape
    /// sequences (`\" \\ \/ \b \f \n \r \t \uXXXX`, incl. surrogate
    /// pairs) until the closing quote.
    fn read_string_escaped(&mut self, start: usize) -> Result<Cow<'a, str>, String> {
        let mut buf: Vec<u8> = Vec::with_capacity((self.pos - start) + 16);
        buf.extend_from_slice(&self.input[start..self.pos]);
        loop {
            match self.input.get(self.pos) {
                None => return Err("unterminated string".to_owned()),
                Some(&b'"') => {
                    self.pos += 1;
                    let s =
                        String::from_utf8(buf).map_err(|_| "invalid UTF-8 in string".to_owned())?;
                    return Ok(Cow::Owned(s));
                }
                Some(&b'\\') => {
                    self.pos += 1;
                    self.decode_escape(&mut buf)?;
                }
                Some(&b) if b < 0x20 => {
                    return Err("control character in string".to_owned());
                }
                Some(&b) => {
                    buf.push(b);
                    self.pos += 1;
                }
            }
        }
    }

    /// Decode the escape sequence whose backslash has already been
    /// consumed, appending the decoded UTF-8 to `buf`.
    fn decode_escape(&mut self, buf: &mut Vec<u8>) -> Result<(), String> {
        let escape = self
            .input
            .get(self.pos)
            .copied()
            .ok_or_else(|| "dangling escape".to_owned())?;
        self.pos += 1;
        match escape {
            b'"' => buf.push(b'"'),
            b'\\' => buf.push(b'\\'),
            b'/' => buf.push(b'/'),
            b'b' => buf.push(0x08),
            b'f' => buf.push(0x0C),
            b'n' => buf.push(0x0A),
            b'r' => buf.push(0x0D),
            b't' => buf.push(0x09),
            b'u' => self.decode_unicode_escape(buf)?,
            _ => return Err("invalid escape".to_owned()),
        }
        Ok(())
    }

    /// Decode a `\uXXXX` escape (the `\u` already consumed), resolving
    /// UTF-16 surrogate pairs and rejecting lone/invalid surrogates.
    fn decode_unicode_escape(&mut self, buf: &mut Vec<u8>) -> Result<(), String> {
        let hi = self.read_hex4()?;
        let code_point = if (0xD800..=0xDBFF).contains(&hi) {
            // High surrogate: must be followed by `\uYYYY` low surrogate.
            if self.input.get(self.pos) != Some(&b'\\')
                || self.input.get(self.pos + 1) != Some(&b'u')
            {
                return Err("unpaired surrogate in unicode escape".to_owned());
            }
            self.pos += 2;
            let lo = self.read_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&lo) {
                return Err("invalid low surrogate in unicode escape".to_owned());
            }
            0x1_0000 + ((u32::from(hi) - 0xD800) << 10) + (u32::from(lo) - 0xDC00)
        } else if (0xDC00..=0xDFFF).contains(&hi) {
            return Err("lone low surrogate in unicode escape".to_owned());
        } else {
            u32::from(hi)
        };
        let ch = char::from_u32(code_point)
            .ok_or_else(|| "invalid code point in unicode escape".to_owned())?;
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
        Ok(())
    }

    /// Read exactly four hex digits as a `u16` (case-insensitive).
    fn read_hex4(&mut self) -> Result<u16, String> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let digit = self
                .input
                .get(self.pos)
                .copied()
                .ok_or_else(|| "truncated unicode escape".to_owned())?;
            let nibble = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return Err("invalid hex digit in unicode escape".to_owned()),
            };
            value = (value << 4) | u16::from(nibble);
            self.pos += 1;
        }
        Ok(value)
    }

    /// Read the `v` field as a `u8` — a non-negative JSON integer in
    /// `[0, 255]`.  Rejects a leading `-`, a fractional/exponent tail,
    /// out-of-range values, and non-numeric tokens (matching serde's
    /// `u8` deserialization decisions).
    fn read_u8(&mut self) -> Result<u8, String> {
        self.skip_ws();
        if self.cur() == Some(b'-') {
            return Err("invalid negative value for `v`".to_owned());
        }
        let mut value: u32 = 0;
        let mut digits = 0u32;
        while let Some(&byte) = self.input.get(self.pos) {
            if byte.is_ascii_digit() {
                value = value
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
                self.pos += 1;
                digits += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err("expected integer for `v`".to_owned());
        }
        if matches!(self.cur(), Some(b'.' | b'e' | b'E')) {
            return Err("invalid non-integer value for `v`".to_owned());
        }
        u8::try_from(value).map_err(|_| "`v` out of range for u8".to_owned())
    }

    /// Consume an arbitrary JSON value (for unknown keys) without
    /// allocating — string-aware so braces/brackets inside strings do
    /// not affect container nesting.
    fn skip_value(&mut self) -> Result<(), String> {
        self.skip_ws();
        match self.cur() {
            Some(b'"') => self.skip_string(),
            Some(b'{' | b'[') => self.skip_container(),
            Some(b't' | b'f' | b'n') => {
                self.skip_literal();
                Ok(())
            }
            Some(b'-' | b'0'..=b'9') => self.skip_number(),
            _ => Err("unexpected value".to_owned()),
        }
    }

    /// Skip a JSON string token (cursor at the opening quote).
    fn skip_string(&mut self) -> Result<(), String> {
        self.pos += 1; // opening quote
        while let Some(&byte) = self.input.get(self.pos) {
            self.pos += 1;
            if byte == b'"' {
                return Ok(());
            }
            if byte == b'\\' && self.input.get(self.pos).is_some() {
                self.pos += 1;
            }
        }
        Err("unterminated string".to_owned())
    }

    /// Skip a balanced `{...}` / `[...]` container (cursor at the opening
    /// bracket), string-literal aware.
    fn skip_container(&mut self) -> Result<(), String> {
        let mut depth = 0usize;
        while let Some(&byte) = self.input.get(self.pos) {
            self.pos += 1;
            match byte {
                b'"' => {
                    // Skip a nested string so its braces don't count.
                    while let Some(&inner) = self.input.get(self.pos) {
                        self.pos += 1;
                        if inner == b'"' {
                            break;
                        }
                        if inner == b'\\' && self.input.get(self.pos).is_some() {
                            self.pos += 1;
                        }
                    }
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err("unterminated container".to_owned())
    }

    /// Skip a JSON literal run (`true` / `false` / `null`).
    fn skip_literal(&mut self) {
        while let Some(&byte) = self.input.get(self.pos) {
            if byte.is_ascii_lowercase() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Skip a JSON number run.
    fn skip_number(&mut self) -> Result<(), String> {
        let start = self.pos;
        while let Some(&byte) = self.input.get(self.pos) {
            if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err("expected number".to_owned());
        }
        Ok(())
    }

    /// Skip JSON whitespace (space, tab, newline, carriage return).
    fn skip_ws(&mut self) {
        while let Some(&byte) = self.input.get(self.pos) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Current byte without advancing.
    fn cur(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Skip whitespace, then return the current byte without advancing.
    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.cur()
    }

    /// Skip whitespace and consume the expected byte, or error.
    fn expect(&mut self, byte: u8) -> Result<(), String> {
        self.skip_ws();
        if self.cur() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", byte as char))
        }
    }

    /// Consume an exact ASCII literal (e.g. `null`), or error.
    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), String> {
        if self.input[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err("invalid literal".to_owned())
        }
    }
}
