//! Hand-rolled parse of the request wire header's optional `principal`
//! field — the authenticated caller set by the trusted host (see
//! [`crate::Principal`]):
//!
//! ```text
//! "principal": null
//!            | { "name": string,                       (required)
//!                "authorities"?: [string, ...],        (default [])
//!                "attributes"?:  {string: string} }    (default {})
//! ```
//!
//! Same contract as the rest of [`super`]: accept/reject- and
//! value-identical to the serde twin (`bench_serde::de_opt_principal`) —
//! any key order, unknown keys validated then skipped, duplicate known keys
//! rejected, plain strings borrowed.  Additionally caps `authorities` and
//! `attributes` at [`MAX_PRINCIPAL_ENTRIES`] each (the serde twin does not;
//! the property tests stay below it), so a hostile host frame cannot grow
//! them without bound.

use std::borrow::Cow;

use super::{CowPairs, Parser, WirePrincipal};

/// Maximum number of `authorities` and of `attributes` entries accepted in
/// one `principal` (each).  Exceeding it is a `400` wire parse error.
pub const MAX_PRINCIPAL_ENTRIES: usize = 1024;

/// `Err("duplicate field `name`")` when `slot` is already filled — serde's
/// derive rejects a repeated known field.
fn reject_duplicate<T>(slot: Option<&T>, field: &str) -> Result<(), String> {
    slot.map_or(Ok(()), |_| Err(format!("duplicate field `{field}`")))
}

impl<'a> Parser<'a> {
    /// Parse the `principal` value: `null` (`None`) or the object above.
    pub(super) fn read_principal(&mut self) -> Result<Option<WirePrincipal<'a>>, String> {
        self.skip_ws();
        if self.cur() == Some(b'n') {
            self.expect_literal(b"null")?;
            return Ok(None);
        }
        self.expect(b'{')?;
        let mut name: Option<Cow<'a, str>> = None;
        let mut authorities: Option<Vec<Cow<'a, str>>> = None;
        let mut attributes: Option<CowPairs<'a>> = None;
        self.skip_ws();
        if self.cur() == Some(b'}') {
            self.pos += 1;
        } else {
            loop {
                let key = self.read_string()?;
                self.expect(b':')?;
                match key.as_ref() {
                    "name" => {
                        reject_duplicate(name.as_ref(), "name")?;
                        name = Some(self.read_string()?);
                    }
                    "authorities" => {
                        reject_duplicate(authorities.as_ref(), "authorities")?;
                        authorities = Some(self.read_bounded(
                            b'[',
                            b']',
                            "authorities",
                            Self::read_string,
                        )?);
                    }
                    "attributes" => {
                        reject_duplicate(attributes.as_ref(), "attributes")?;
                        attributes = Some(self.read_bounded(b'{', b'}', "attributes", |p| {
                            let key = p.read_string()?;
                            p.expect(b':')?;
                            Ok((key, p.read_string()?))
                        })?);
                    }
                    _ => self.skip_value()?,
                }
                if self.close_or_continue(b'}', "object")? {
                    break;
                }
            }
        }
        Ok(Some(WirePrincipal {
            name: name.ok_or_else(|| "missing field `name` in `principal`".to_owned())?,
            authorities: authorities.unwrap_or_default(),
            attributes: attributes.unwrap_or_default(),
        }))
    }

    /// Parse an `open … close` container whose elements `item` reads,
    /// rejecting more than [`MAX_PRINCIPAL_ENTRIES`] elements.
    fn read_bounded<T>(
        &mut self,
        open: u8,
        close: u8,
        what: &str,
        mut item: impl FnMut(&mut Self) -> Result<T, String>,
    ) -> Result<Vec<T>, String> {
        self.expect(open)?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.cur() == Some(close) {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            if out.len() == MAX_PRINCIPAL_ENTRIES {
                return Err(format!(
                    "too many principal {what} (more than {MAX_PRINCIPAL_ENTRIES})"
                ));
            }
            out.push(item(self)?);
            if self.close_or_continue(close, what)? {
                return Ok(out);
            }
        }
    }

    /// After an element: consume `,` (→ `false`, more follow) or `close`
    /// (→ `true`, container done); anything else is an error.
    fn close_or_continue(&mut self, close: u8, what: &str) -> Result<bool, String> {
        self.skip_ws();
        match self.cur() {
            Some(b',') => {
                self.pos += 1;
                Ok(false)
            }
            Some(byte) if byte == close => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(format!(
                "expected ',' or '{}' in principal {what}",
                close as char
            )),
        }
    }
}
