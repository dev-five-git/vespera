//! Reflect field-level `#[garde(...)]` rules into the field's JSON Schema.
//!
//! A type that derives `garde::Validate` itself (for context-aware rules —
//! `#[garde(context(..))]` + `ValidatedWith`) carries its rules in
//! `#[garde(...)]` attributes that `#[derive(Schema)]` sees but garde owns:
//! garde's derive generates the runtime check, and this module documents
//! every rule with a deterministic JSON Schema equivalent so `openapi.json`
//! states the same constraints the `422` enforces.
//!
//! | garde rule | JSON Schema |
//! |---|---|
//! | `length(chars, min, max, equal)` on a string | `minLength` / `maxLength` (both count Unicode scalar values) |
//! | `length(min, max)` on a string in any other mode (`simple` — the default — `bytes`, `graphemes`, `utf16`) | only `min <= 1` → `minLength` (empty ⇔ zero units in every mode); a larger byte / grapheme / UTF-16 bound has no code-point equivalent and is skipped |
//! | `length(...)` (default `simple` mode) on an array / map | `minItems` / `maxItems`, `minProperties` / `maxProperties` |
//! | `range(min, max, equal)` with numeric literals | `minimum` / `maximum` (garde's range is inclusive) |
//! | `email` / `url` / `ipv4` / `ipv6` | `format: email` / `uri` / `ipv4` / `ipv6` |
//! | `ip` | `anyOf: [{format: ipv4}, {format: ipv6}]` |
//! | `pattern("literal")` | `pattern` |
//! | `prefix("x")` / `suffix("x")` / `contains("x")` | `pattern` `^x` / `x$` / `x` (regex metacharacters escaped) |
//! | `ascii` | `pattern: ^[\x00-\x7F]*$` |
//! | `inner(...)` on `Vec<T>` / `[T; N]` | the rules land on `items`; on `Option<T>` on the value itself |
//!
//! The first pattern occupies `pattern`; further ones are ANDed via `allOf`
//! members. Rules on an `Option<T>` field constrain the non-null value
//! (garde skips `None`; the keywords above ignore `null`).
//!
//! **Skipped** (no deterministic equivalent): `alphanumeric` (Unicode
//! `char::is_alphanumeric`, not expressible without `\p{..}` regex support),
//! `required` (one component documents both request and response — a response
//! may still serialize `None` as `null`), `credit_card`, `phone_number`,
//! `matches(field)`, `custom(..)`, `if(..)`, `dive`, `skip`, non-literal
//! arguments (consts / static regexes), and `inner` on maps (garde has no map
//! `Inner` impl).
//!
//! **Explicit wins:** a keyword the field's `#[schema(...)]` sets is never
//! derived (a derived pattern then goes to `allOf` instead). Bounds the type
//! already implies (`u32` → `minimum: 0`) are tightened, never loosened.
//! Unparseable `#[garde]` attributes are ignored — garde's own derive reports
//! them.

use syn::{Attribute, Expr, ExprLit, Lit, Type, meta::ParseNestedMeta};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::schema_attrs::{SchemaConstraints, expr_to_f64};
use crate::schema_macro::type_utils::{first_generic_type_arg, last_path_segment, option_inner};

/// `pattern` documenting garde's `ascii` rule.
const ASCII_PATTERN: &str = r"^[\x00-\x7F]*$";

/// garde's length unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LengthMode {
    /// `simple` (the default): `len()` — bytes for strings, count otherwise.
    #[default]
    Simple,
    /// `chars`: Unicode scalar values — JSON Schema's string length unit.
    Chars,
    /// `bytes` / `graphemes` / `utf16`.
    Other,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct LengthRule {
    mode: LengthMode,
    min: Option<usize>,
    max: Option<usize>,
}

impl LengthRule {
    /// `(minLength, maxLength)` for a string; see the module table.
    fn string_bounds(&self) -> (Option<usize>, Option<usize>) {
        if self.mode == LengthMode::Chars {
            (self.min, self.max)
        } else {
            (self.min.filter(|min| *min <= 1), None)
        }
    }

    /// `(min, max)` element count for an array / map (`simple` mode only).
    fn count_bounds(&self) -> (Option<usize>, Option<usize>) {
        if self.mode == LengthMode::Simple {
            (self.min, self.max)
        } else {
            (None, None)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatRule {
    Email,
    Url,
    Ip,
    Ipv4,
    Ipv6,
}

/// The schema-relevant subset of a field's `#[garde(...)]` rules.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GardeRules {
    length: Option<LengthRule>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    format: Option<FormatRule>,
    patterns: Vec<String>,
    inner: Option<Box<Self>>,
}

/// Read every `#[garde(...)]` attribute in `attrs` (lenient: an attribute this
/// reader cannot parse contributes nothing).
#[must_use]
pub fn extract_garde_rules(attrs: &[Attribute]) -> GardeRules {
    let mut rules = GardeRules::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("garde")) {
        let before = rules.clone();
        if attr
            .parse_nested_meta(|meta| parse_rule(&mut rules, &meta))
            .is_err()
        {
            rules = before;
        }
    }
    rules
}

fn parse_rule(rules: &mut GardeRules, meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    let path = &meta.path;
    if path.is_ident("length") {
        let mut length = LengthRule::default();
        meta.parse_nested_meta(|arg| parse_length_arg(&mut length, &arg))?;
        rules.length = Some(length);
    } else if path.is_ident("range") {
        meta.parse_nested_meta(|arg| parse_range_arg(rules, &arg))?;
    } else if path.is_ident("email") {
        rules.format = Some(FormatRule::Email);
    } else if path.is_ident("url") {
        rules.format = Some(FormatRule::Url);
    } else if path.is_ident("ip") {
        rules.format = Some(FormatRule::Ip);
    } else if path.is_ident("ipv4") {
        rules.format = Some(FormatRule::Ipv4);
    } else if path.is_ident("ipv6") {
        rules.format = Some(FormatRule::Ipv6);
    } else if path.is_ident("ascii") {
        rules.patterns.push(ASCII_PATTERN.to_owned());
    } else if path.is_ident("pattern") {
        rules.patterns.extend(string_arg(meta)?);
    } else if path.is_ident("prefix") {
        rules
            .patterns
            .extend(string_arg(meta)?.map(|s| format!("^{}", escape_regex(&s))));
    } else if path.is_ident("suffix") {
        rules
            .patterns
            .extend(string_arg(meta)?.map(|s| format!("{}$", escape_regex(&s))));
    } else if path.is_ident("contains") {
        rules
            .patterns
            .extend(string_arg(meta)?.map(|s| escape_regex(&s)));
    } else if path.is_ident("inner") {
        let inner: &mut GardeRules = rules.inner.get_or_insert_with(Box::default);
        meta.parse_nested_meta(|nested| parse_rule(inner, &nested))?;
    } else {
        skip_payload(meta)?;
    }
    Ok(())
}

fn parse_length_arg(length: &mut LengthRule, arg: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if arg.path.is_ident("min") {
        length.min = usize_arg(arg)?;
    } else if arg.path.is_ident("max") {
        length.max = usize_arg(arg)?;
    } else if arg.path.is_ident("equal") {
        let equal = usize_arg(arg)?;
        length.min = equal;
        length.max = equal;
    } else if arg.path.is_ident("chars") {
        length.mode = LengthMode::Chars;
    } else if !arg.path.is_ident("simple") {
        length.mode = LengthMode::Other;
    }
    Ok(())
}

fn parse_range_arg(rules: &mut GardeRules, arg: &ParseNestedMeta<'_>) -> syn::Result<()> {
    let is_min = arg.path.is_ident("min");
    let is_max = arg.path.is_ident("max");
    let is_equal = arg.path.is_ident("equal");
    let value = expr_to_f64(arg.value()?.parse()?).ok();
    if is_min || is_equal {
        rules.minimum = value;
    }
    if is_max || is_equal {
        rules.maximum = value;
    }
    Ok(())
}

/// `= <int literal>`; a non-literal (a const) has no static value → `None`.
fn usize_arg(arg: &ParseNestedMeta<'_>) -> syn::Result<Option<usize>> {
    let expr: Expr = arg.value()?.parse()?;
    Ok(match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(int), ..
        }) => int.base10_parse().ok(),
        _ => None,
    })
}

/// `("<string literal>")`; any other argument (a static regex) → `None`.
fn string_arg(meta: &ParseNestedMeta<'_>) -> syn::Result<Option<String>> {
    let content;
    syn::parenthesized!(content in meta.input);
    let expr: Expr = content.parse()?;
    Ok(match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Some(s.value()),
        _ => None,
    })
}

/// Consume an unmapped rule's `(...)` or `= value` payload.
fn skip_payload(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        content.parse::<proc_macro2::TokenStream>()?;
    } else if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<Expr>()?;
    }
    Ok(())
}

/// Escape ECMA-262 syntax characters so `literal` matches itself (valid with
/// and without the `u` flag).
fn escape_regex(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for ch in literal.chars() {
        if matches!(
            ch,
            '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Apply `rules` to the schema of a field of type `ty`. `$ref` schemas (and
/// `allOf` wrappers around one) carry no type to constrain and are left as is.
pub fn apply_garde_rules(
    schema_ref: &mut SchemaRef,
    ty: &Type,
    rules: &GardeRules,
    explicit: &SchemaConstraints,
) {
    if let SchemaRef::Inline(schema) = schema_ref {
        apply_to_schema(schema, ty, rules, explicit);
    }
}

fn apply_to_schema(
    schema: &mut Schema,
    ty: &Type,
    rules: &GardeRules,
    explicit: &SchemaConstraints,
) {
    match schema.schema_type {
        Some(SchemaType::String) => apply_string_rules(schema, rules, explicit),
        Some(SchemaType::Array) => {
            let (min, max) = count_bounds(rules);
            tighten(&mut schema.min_items, explicit.min_items, min, true);
            tighten(&mut schema.max_items, explicit.max_items, max, false);
        }
        Some(SchemaType::Object) => {
            let (min, max) = count_bounds(rules);
            tighten(&mut schema.min_properties, None, min, true);
            tighten(&mut schema.max_properties, None, max, false);
        }
        Some(SchemaType::Integer | SchemaType::Number) => {
            tighten(&mut schema.minimum, explicit.minimum, rules.minimum, true);
            tighten(&mut schema.maximum, explicit.maximum, rules.maximum, false);
        }
        _ => {}
    }
    let no_explicit = SchemaConstraints::default();
    if let Some(inner) = &rules.inner
        && let Some((elem_ty, on_items)) = inner_target(ty)
    {
        if !on_items {
            apply_to_schema(schema, elem_ty, inner, &no_explicit);
        } else if let Some(SchemaRef::Inline(items)) = schema.items.as_mut() {
            apply_to_schema(items, elem_ty, inner, &no_explicit);
        }
    }
}

fn count_bounds(rules: &GardeRules) -> (Option<usize>, Option<usize>) {
    rules
        .length
        .as_ref()
        .map_or((None, None), LengthRule::count_bounds)
}

fn apply_string_rules(schema: &mut Schema, rules: &GardeRules, explicit: &SchemaConstraints) {
    let (min, max) = rules
        .length
        .as_ref()
        .map_or((None, None), LengthRule::string_bounds);
    tighten(&mut schema.min_length, explicit.min_length, min, true);
    tighten(&mut schema.max_length, explicit.max_length, max, false);

    if let Some(format) = rules.format
        && schema.format.is_none()
        && explicit.format.is_none()
    {
        match format {
            FormatRule::Email => schema.format = Some("email".to_owned()),
            FormatRule::Url => schema.format = Some("uri".to_owned()),
            FormatRule::Ipv4 => schema.format = Some("ipv4".to_owned()),
            FormatRule::Ipv6 => schema.format = Some("ipv6".to_owned()),
            FormatRule::Ip => {
                schema.any_of = Some(vec![format_only("ipv4"), format_only("ipv6")]);
            }
        }
    }

    let mut patterns = rules.patterns.iter().cloned();
    if explicit.pattern.is_none() && schema.pattern.is_none() {
        schema.pattern = patterns.next();
    }
    let extra: Vec<SchemaRef> = patterns
        .map(|pattern| {
            SchemaRef::Inline(Box::new(Schema {
                pattern: Some(pattern),
                ..Schema::default()
            }))
        })
        .collect();
    if !extra.is_empty() {
        schema.all_of.get_or_insert_with(Vec::new).extend(extra);
    }
}

fn format_only(format: &str) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: Some(format.to_owned()),
        ..Schema::default()
    }))
}

/// Set `slot` from `derived` unless the field's `#[schema]` sets the keyword
/// (`explicit`); an existing type-implied bound is only ever tightened.
fn tighten<T: Copy + PartialOrd>(
    slot: &mut Option<T>,
    explicit: Option<T>,
    derived: Option<T>,
    lower_bound: bool,
) {
    if explicit.is_none()
        && let Some(value) = derived
    {
        *slot = Some(match *slot {
            Some(current) if (current > value) == lower_bound && current != value => current,
            _ => value,
        });
    }
}

/// Where garde's `inner` rules apply: the `Option` value itself (`false`) or
/// the elements of a `Vec` / array (`true`).
fn inner_target(ty: &Type) -> Option<(&Type, bool)> {
    if let Some(inner) = option_inner(ty) {
        return Some((inner, false));
    }
    if let Type::Array(array) = ty {
        return Some((&array.elem, true));
    }
    let segment = last_path_segment(ty).filter(|segment| segment.ident == "Vec")?;
    first_generic_type_arg(segment).map(|elem| (elem, true))
}

#[cfg(test)]
mod tests;
