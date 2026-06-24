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

/// Container-nesting levels tracked **inline** (zero-allocation) while
/// skipping the value of an unknown (forward-compat) header field, before
/// the rare deep-nesting spill to a heap `Vec`.  128 covers every realistic
/// forward-compat value; the unknown-value skip is *iterative* (see
/// [`Parser::skip_value`]) so deeper nesting is still accepted exactly as
/// `serde_json`'s iterative `ignore_value` does — never via native
/// recursion, so hostile depth can never overflow the stack and crash the
/// host JVM across the JNI boundary (a stack overflow is NOT catchable by
/// the `catch_unwind` guards at the JNI entry points).
const INLINE_SKIP_DEPTH: usize = 128;

/// Initial capacity for the request-header `(name, value)` pair `Vec`.
///
/// Sized for a realistic browser / reverse-proxy / API request header set
/// (host, user-agent, accept*, content-type, authorization, cookie,
/// forwarded / trace headers, cache-control, ...) so the common case fills
/// without a single reallocation.  The previous capacity of `8` reallocated
/// once at the 9th header — the exact realloc the 16-header
/// `tests/alloc_budget.rs` Case C documented.  A small request transiently
/// over-reserves a few hundred bytes (same alloc *count*); removing the
/// realloc on the larger, common request shape is the priority (speed first).
const TYPICAL_HEADER_CAP: usize = 16;

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
                    // Unknown (forward-compat) key: iteratively
                    // validate-and-skip its value (no native recursion).
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
        // Pre-reserve for a typical request's header count so the pushes
        // don't trigger the Vec's early doubling reallocations (the previous
        // `Vec::new()` reallocated at 1, 2, 4, 8, ...).  See
        // [`TYPICAL_HEADER_CAP`] for the chosen size and rationale.
        let mut out: CowPairs<'a> = Vec::with_capacity(TYPICAL_HEADER_CAP);
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
        // Scalar single-pass scan.  A `memchr2(b'"', b'\\')` variant was
        // benchmarked (2026) and REGRESSED `request_parse_hand` ~13% and
        // `request_headers_path` ~3-4%: header values are short, so the
        // SIMD setup cost plus a second control-character pass over the
        // span outweighs the vectorised search.  The branchy scalar loop
        // wins for the small-string sizes this parser actually sees.
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
        // Reserve ~2× the already-scanned plain prefix (+16 floor): an
        // escaped value's tail is typically the same order of magnitude as
        // its plain head, so this absorbs most of it without the doubling
        // reallocations the old flat `+16` paid on longer escaped values.
        // Sizing off the prefix (never the total input length) keeps a
        // short escaped value early in a large request from over-reserving.
        let prefix = self.pos - start;
        let mut buf: Vec<u8> = Vec::with_capacity(prefix.saturating_mul(2).saturating_add(16));
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
    /// `[0, 255]`.  Rejects a leading `-`, a **leading zero** (`01`, `00`
    /// — JSON forbids them, only a bare `0` is legal), a
    /// fractional/exponent tail, out-of-range values, and non-numeric
    /// tokens (matching serde's `u8` deserialization decisions).
    fn read_u8(&mut self) -> Result<u8, String> {
        self.skip_ws();
        if self.cur() == Some(b'-') {
            return Err("invalid negative value for `v`".to_owned());
        }
        // JSON forbids leading zeros: a `0` may only stand alone, never be
        // followed by another digit.  `serde_json` rejects `01`/`00`.
        let first_is_zero = self.cur() == Some(b'0');
        let mut value: u32 = 0;
        let mut digits = 0u32;
        while let Some(&byte) = self.input.get(self.pos) {
            if byte.is_ascii_digit() {
                value = value
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
                self.pos += 1;
                digits += 1;
                // Defense-in-depth: `v` must fit in u8 (the wire protocol
                // version, in practice `1`). Once the accumulator exceeds 255
                // the value is already out of range, so stop instead of
                // consuming a pathologically long digit run (bounded by the
                // 1 MiB header cap, but still up to ~1M wasted iterations on
                // hostile `"v":999…9` input). `u8::try_from` below would reject
                // it anyway, so this is accept/reject-identical to serde — the
                // value can never round-trip to a valid `u8`.
                if value > u32::from(u8::MAX) {
                    return Err("`v` out of range for u8".to_owned());
                }
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err("expected integer for `v`".to_owned());
        }
        if first_is_zero && digits > 1 {
            return Err("invalid leading zero in `v`".to_owned());
        }
        if matches!(self.cur(), Some(b'.' | b'e' | b'E')) {
            return Err("invalid non-integer value for `v`".to_owned());
        }
        u8::try_from(value).map_err(|_| "`v` out of range for u8".to_owned())
    }

    /// Iteratively **validate-and-skip** one JSON value — the value of an
    /// unknown (forward-compat) header field — enforcing `serde_json`'s full
    /// grammar (including bracket matching) so a malformed value under an
    /// ignored key is rejected, not silently skipped.
    ///
    /// Matches `serde_json`'s `ignore_value`: nesting is walked with an
    /// explicit container-type stack ([`ContainerStack`]) instead of native
    /// recursion, so an arbitrarily deep value is accepted/rejected exactly
    /// as serde does WITHOUT ever overflowing the native stack (which would
    /// crash the host JVM across the JNI boundary, uncatchable by
    /// `catch_unwind`).  Allocates nothing for the common shallow value: the
    /// stack is inline for the first [`INLINE_SKIP_DEPTH`] levels and the
    /// non-allocating [`Self::skip_string`] is used throughout.
    fn skip_value(&mut self) -> Result<(), String> {
        let mut stack = ContainerStack::new();
        loop {
            // ── Parse one value at the current position. ──
            self.skip_ws();
            match self.cur() {
                Some(b'{') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.cur() == Some(b'}') {
                        self.pos += 1; // empty object: a complete value
                    } else {
                        stack.push(true);
                        self.skip_string()?; // first key
                        self.expect(b':')?;
                        continue; // descend to parse its value
                    }
                }
                Some(b'[') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.cur() == Some(b']') {
                        self.pos += 1; // empty array: a complete value
                    } else {
                        stack.push(false);
                        continue; // descend to parse the first element
                    }
                }
                Some(b'"') => self.skip_string()?,
                Some(b't') => self.expect_literal(b"true")?,
                Some(b'f') => self.expect_literal(b"false")?,
                Some(b'n') => self.expect_literal(b"null")?,
                Some(b'-' | b'0'..=b'9') => self.skip_number()?,
                _ => return Err("unexpected value".to_owned()),
            }
            // ── A complete value was parsed.  Ascend: step past commas to
            // the next sibling, or pop finished containers.  An empty stack
            // means the whole top-level value is done. ──
            loop {
                let Some(is_object) = stack.top() else {
                    return Ok(());
                };
                self.skip_ws();
                match self.cur() {
                    Some(b',') => {
                        self.pos += 1;
                        if is_object {
                            self.skip_ws();
                            self.skip_string()?; // next key
                            self.expect(b':')?;
                        }
                        break; // parse the next value / element
                    }
                    Some(b'}') if is_object => {
                        self.pos += 1;
                        stack.pop();
                    }
                    Some(b']') if !is_object => {
                        self.pos += 1;
                        stack.pop();
                    }
                    _ => {
                        return Err(if is_object {
                            "expected ',' or '}' in object".to_owned()
                        } else {
                            "expected ',' or ']' in array".to_owned()
                        });
                    }
                }
            }
        }
    }

    /// Validate-and-skip a JSON string (cursor at the opening quote)
    /// **without allocating** — the byte-for-byte accept/reject twin of
    /// [`Self::read_string`] (escape set, unescaped control-character
    /// rejection, UTF-8 validation, surrogate-pair rules) that discards the
    /// value instead of decoding it into a `String`.
    ///
    /// The previous implementation delegated to `read_string`, paying a
    /// throwaway heap `String` decode for an escaped string under an ignored
    /// key.  This scans in place: every unescaped run is UTF-8-validated
    /// against the source bytes (a multi-byte UTF-8 sequence never straddles
    /// a `\`-escape, so per-run validation equals validating the whole
    /// decoded string) and every escape is validated, never decoded.
    fn skip_string(&mut self) -> Result<(), String> {
        self.skip_ws();
        if self.cur() != Some(b'"') {
            return Err("expected string".to_owned());
        }
        self.pos += 1;
        let input = self.input;
        // Start of the current unescaped byte run, UTF-8-validated when it
        // ends (at the closing quote or the next escape).
        let mut run_start = self.pos;
        loop {
            match input.get(self.pos) {
                None => return Err("unterminated string".to_owned()),
                Some(&b'"') => {
                    std::str::from_utf8(&input[run_start..self.pos])
                        .map_err(|_| "invalid UTF-8 in string".to_owned())?;
                    self.pos += 1;
                    return Ok(());
                }
                Some(&b'\\') => {
                    std::str::from_utf8(&input[run_start..self.pos])
                        .map_err(|_| "invalid UTF-8 in string".to_owned())?;
                    self.pos += 1;
                    self.validate_escape()?;
                    run_start = self.pos;
                }
                Some(&b) if b < 0x20 => {
                    return Err("control character in string".to_owned());
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Validate (but do not decode) the escape sequence whose backslash has
    /// already been consumed — the non-allocating twin of
    /// [`Self::decode_escape`], used by [`Self::skip_string`].
    fn validate_escape(&mut self) -> Result<(), String> {
        let escape = self
            .input
            .get(self.pos)
            .copied()
            .ok_or_else(|| "dangling escape".to_owned())?;
        self.pos += 1;
        match escape {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => Ok(()),
            b'u' => self.validate_unicode_escape(),
            _ => Err("invalid escape".to_owned()),
        }
    }

    /// Validate a `\uXXXX` escape (the `\u` already consumed), enforcing the
    /// same surrogate-pair rules as [`Self::decode_unicode_escape`] without
    /// computing the code point.  A validated high+low pair always forms a
    /// scalar (`<= 0x10FFFF`) and a non-surrogate BMP unit is always a
    /// scalar, so the decoder's `char::from_u32` check can never reject here
    /// — accept/reject parity with `decode_unicode_escape` is preserved.
    fn validate_unicode_escape(&mut self) -> Result<(), String> {
        let hi = self.read_hex4()?;
        if (0xD800..=0xDBFF).contains(&hi) {
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
            Ok(())
        } else if (0xDC00..=0xDFFF).contains(&hi) {
            Err("lone low surrogate in unicode escape".to_owned())
        } else {
            Ok(())
        }
    }

    /// Validate-and-skip a JSON number, enforcing the JSON number grammar
    /// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?` so malformed
    /// numbers like `1e+`, `1.`, or a leading-zero `01` are rejected the
    /// same way `serde_json` rejects them.  (A leading-zero integer such
    /// as `01` consumes the `0` and leaves the `1`, so the surrounding
    /// container's delimiter check rejects it — matching serde.)
    fn skip_number(&mut self) -> Result<(), String> {
        if self.cur() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part: a bare `0`, or `[1-9][0-9]*` (no leading zero).
        match self.cur() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.cur(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid number: expected a digit".to_owned()),
        }
        // Optional fraction: `.` then at least one digit.
        if self.cur() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.cur(), Some(b'0'..=b'9')) {
                return Err("invalid number: expected a digit after '.'".to_owned());
            }
            while matches!(self.cur(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        // Optional exponent: `e`/`E`, optional sign, at least one digit.
        if matches!(self.cur(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.cur(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.cur(), Some(b'0'..=b'9')) {
                return Err("invalid number: expected a digit in the exponent".to_owned());
            }
            while matches!(self.cur(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
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
        // `self.input.get(self.pos..)` instead of `self.input[self.pos..]`:
        // the parser invariant keeps `pos <= len`, but the slice index would
        // panic if a future edit ever broke it. `.get` returns `None` past the
        // end, which folds into the error arm — byte-identical for every
        // reachable state, panic-free if the invariant is ever violated.
        if self
            .input
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(literal))
        {
            self.pos += literal.len();
            Ok(())
        } else {
            Err("invalid literal".to_owned())
        }
    }
}

/// Explicit open-container stack for the iterative unknown-value skip in
/// [`Parser::skip_value`]: one bit per open container (`true` = object,
/// `false` = array) so a `]` is validated to close an array and a `}` an
/// object (matching `serde_json`'s grammar).
///
/// The first [`INLINE_SKIP_DEPTH`] levels live in an inline bitset, so the
/// overwhelmingly common shallow value skips **without allocating**; only
/// pathologically deep nesting (reachable solely from hostile input) spills
/// to the heap `overflow` vec — and even then the walk stays iterative, so
/// the native stack is never at risk.
struct ContainerStack {
    inline: [u64; INLINE_SKIP_DEPTH / 64],
    depth: usize,
    overflow: Vec<bool>,
}

impl ContainerStack {
    fn new() -> Self {
        Self {
            inline: [0; INLINE_SKIP_DEPTH / 64],
            depth: 0,
            overflow: Vec::new(),
        }
    }

    /// Push a newly-opened container (`is_object` selects `{` vs `[`).
    fn push(&mut self, is_object: bool) {
        if self.depth < INLINE_SKIP_DEPTH {
            let (word, bit) = (self.depth / 64, self.depth % 64);
            if is_object {
                self.inline[word] |= 1u64 << bit;
            } else {
                self.inline[word] &= !(1u64 << bit);
            }
        } else {
            self.overflow.push(is_object);
        }
        self.depth += 1;
    }

    /// Pop the innermost container (no-op when already empty).
    fn pop(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth >= INLINE_SKIP_DEPTH {
            self.overflow.pop();
        }
    }

    /// The innermost open container's type (`Some(true)` = object,
    /// `Some(false)` = array), or `None` when the stack is empty.
    fn top(&self) -> Option<bool> {
        if self.depth == 0 {
            return None;
        }
        let idx = self.depth - 1;
        if idx < INLINE_SKIP_DEPTH {
            let (word, bit) = (idx / 64, idx % 64);
            Some(self.inline[word] & (1u64 << bit) != 0)
        } else {
            self.overflow.last().copied()
        }
    }
}
