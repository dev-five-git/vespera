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
//! | several `length(..)` rules (one per mode) | every rule's bounds, tightest wins |
//! | `range(min, max, equal)` with numeric literals | `minimum` / `maximum` (garde's range is inclusive) |
//! | `email` / `url` / `ipv4` / `ipv6` | `format: email` / `uri` / `ipv4` / `ipv6` |
//! | `ip` | `anyOf: [{format: ipv4}, {format: ipv6}]` |
//! | `pattern("literal")` | `pattern` (skipped when it uses Rust-only regex syntax, see [`is_ecma_compatible`]) |
//! | `prefix("x")` / `suffix("x")` / `contains("x")` | `pattern` `^x` / `x$` / `x` (regex metacharacters escaped) |
//! | `ascii` | `pattern: ^[\x00-\x7F]*$` |
//! | `inner(...)` on `Vec<T>` / `[T; N]` / `Box<[T]>` / `&[T]` | the rules land on `items`; on `Option<T>` on the value itself |
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
    /// One per mode: garde rejects only a repeated mode
    /// (`length(chars, ..), length(bytes, ..)` both apply).
    length: Vec<LengthRule>,
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
        rules.length.push(length);
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
        rules
            .patterns
            .extend(string_arg(meta)?.filter(|pattern| is_ecma_compatible(pattern)));
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
            for (min, max) in rules.length.iter().map(LengthRule::count_bounds) {
                tighten(&mut schema.min_items, explicit.min_items, min, true);
                tighten(&mut schema.max_items, explicit.max_items, max, false);
            }
        }
        Some(SchemaType::Object) => {
            for (min, max) in rules.length.iter().map(LengthRule::count_bounds) {
                tighten(&mut schema.min_properties, None, min, true);
                tighten(&mut schema.max_properties, None, max, false);
            }
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
        // `Option<T>`: the rules constrain this same schema, so the field's
        // explicit `#[schema(..)]` keywords still win; items have none.
        if !on_items {
            apply_to_schema(schema, elem_ty, inner, explicit);
        } else if let Some(SchemaRef::Inline(items)) = schema.items.as_mut() {
            apply_to_schema(items, elem_ty, inner, &no_explicit);
        }
    }
}

fn apply_string_rules(schema: &mut Schema, rules: &GardeRules, explicit: &SchemaConstraints) {
    for (min, max) in rules.length.iter().map(LengthRule::string_bounds) {
        tighten(&mut schema.min_length, explicit.min_length, min, true);
        tighten(&mut schema.max_length, explicit.max_length, max, false);
    }

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
/// the elements of a `Vec` / array / `Box<[T]>` / `&[T]` (`true`) — garde's
/// `Inner` impls.
fn inner_target(ty: &Type) -> Option<(&Type, bool)> {
    if let Some(inner) = option_inner(ty) {
        return Some((inner, false));
    }
    match ty {
        Type::Array(array) => return Some((&array.elem, true)),
        Type::Reference(reference) => return slice_elem(&reference.elem),
        _ => {}
    }
    let segment = last_path_segment(ty)?;
    let elem = first_generic_type_arg(segment)?;
    if segment.ident == "Vec" {
        Some((elem, true))
    } else if segment.ident == "Box" {
        slice_elem(elem)
    } else {
        None
    }
}

/// `T` of a slice type `[T]`, as an `inner` target.
fn slice_elem(ty: &Type) -> Option<(&Type, bool)> {
    match ty {
        Type::Slice(slice) => Some((&slice.elem, true)),
        _ => None,
    }
}

/// Whether a garde `pattern` (Rust `regex` syntax) reads the same as an
/// ECMA-262 `pattern`. A Rust-only construct — an inline flag group (`(?i)`,
/// `(?x:..)`), `(?P<name>..)`, `\A` / `\z`, `\x{..}`, a one-letter `\pL`,
/// the word-boundary escapes `\<` / `\>` / `\b{..}`, the escapes `\U..` /
/// `\u{..}` / `\a`, an escape outside the `u`-mode allowlist (Rust accepts
/// any escaped punctuation — `\-` outside a class, `\#`, `\~`, ... — which
/// `u` mode rejects, see [`ecma_escape`]), a `\p{..}` name ECMA-262 spells
/// differently, a `]` or a `}` that closes nothing (a literal in Rust, a
/// syntax error in ECMA-262's `u` mode), a `{..}` holding anything but
/// `digits[,[digits]]` (Rust skips whitespace in `\d{3, 4}`), or inside a character class a nested class / POSIX class (`[a[b]]`,
/// `[[:alpha:]]`), a set operation (`&&`, `--`, `~~`) or a leading `]`
/// (literal in Rust, an empty class in ECMA-262) — makes it skipped rather
/// than documented wrongly. Known limitations, where the documented pattern
/// may be stricter than garde: `\d` / `\w` / `\b` are Unicode-aware in Rust
/// but ASCII in ECMA-262; `.` matches anything but `\n` in Rust, while
/// ECMA-262 also excludes `\r`, U+2028 and U+2029 (`^.+$` rejects `"a\rb"`,
/// which garde accepts). `\s` is Unicode whitespace in both, but the sets
/// differ by U+0085 (Rust only) and U+FEFF (ECMA-262 only).
fn is_ecma_compatible(pattern: &str) -> bool {
    let mut chars = pattern.chars().peekable();
    // Open `(` groups outside a class: unbalanced parentheses (and `)` with
    // nothing open) are syntax errors in both dialects.
    let mut groups: i64 = 0;
    let mut in_class = false;
    // Right after a class's `[` / `[^`.
    let mut class_start = false;
    // Inside a `{..}` outside a class.
    let mut in_brace = false;
    // Inside that `{..}`: a digit was read / the `,` was read.
    let (mut brace_digit, mut brace_comma) = (false, false);
    while let Some(ch) = chars.next() {
        // Rust's counted repetition skips whitespace (`\d{3, 4}`, `a{ 5 }`);
        // ECMA-262 reads only `digits[,[digits]]` there.
        if in_brace && ch != '}' {
            if ch.is_ascii_digit() {
                brace_digit = true;
            } else if ch != ',' || !brace_digit || std::mem::replace(&mut brace_comma, true) {
                return false;
            }
            continue;
        }
        let next = chars.peek().copied();
        let first = std::mem::take(&mut class_start);
        let rust_only = match (ch, next) {
            // A trailing `\` escapes nothing.
            ('\\', None) => return false,
            ('\\', Some(escaped)) => {
                chars.next();
                !ecma_escape(escaped, in_class, &mut chars)
            }
            ('(', Some('?')) if !in_class => {
                chars.next();
                groups += 1;
                chars.peek().is_some_and(|flag| "imsxuUR-P".contains(*flag))
            }
            ('(', _) if !in_class => {
                groups += 1;
                false
            }
            (')', _) if !in_class => {
                groups -= 1;
                groups < 0
            }
            // Rust opens a nested (or POSIX) class; ECMA-262 reads a literal.
            ('[', _) if in_class => true,
            ('[', _) => {
                in_class = true;
                class_start = true;
                chars.next_if_eq(&'^');
                false
            }
            (']', _) if in_class => {
                in_class = false;
                first
            }
            // A literal in Rust, a syntax error in ECMA-262's `u` mode.
            (']', _) => true,
            ('&' | '-' | '~', Some(op)) => in_class && op == ch,
            ('{', _) if !in_class => {
                (in_brace, brace_digit, brace_comma) = (true, false, false);
                false
            }
            // `{}` holds no count: ECMA-262 reads a quantifier only with digits.
            ('}', _) if !in_class => {
                let counted = std::mem::take(&mut in_brace) && brace_digit;
                !counted
            }
            _ => false,
        };
        if rust_only {
            return false;
        }
    }
    // Every class, `{..}` quantifier and group must be closed.
    !in_class && !in_brace && groups == 0
}

/// Whether `\<escaped>` (the `\` and `escaped` already consumed) is an escape
/// ECMA-262's `u` mode accepts with the meaning Rust gives it. An allowlist:
/// the syntax characters and `/` (plus `-` inside a class), the class /
/// control escapes, `\x..` / `\u....` without braces, and a `\p{..}` /
/// `\P{..}` (its `{name}` consumed here) whose name both engines spell alike.
fn ecma_escape(
    escaped: char,
    in_class: bool,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> bool {
    let brace = chars.peek() == Some(&'{');
    match escaped {
        // The syntax characters and `/`, then the class / control escapes
        // (a `{` after these is a quantifier: `\d{4}`).
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        | '/' | 'd' | 'D' | 'w' | 'W' | 's' | 'S' | 'B' | 'n' | 'r' | 't' | 'f' | 'v' => true,
        '-' => in_class,
        // Rust's `\b{start}` / `\x{41}` / `\u{41}` (ECMA-262: no braces).
        'b' | 'x' | 'u' => !brace,
        'p' | 'P' if brace => {
            chars.next();
            let mut closed = false;
            let name: String = chars
                .by_ref()
                .take_while(|ch| {
                    closed = *ch == '}';
                    !closed
                })
                .collect();
            closed && is_ecma_property(&name)
        }
        _ => false,
    }
}

/// ECMA-262 `General_Category` values (long and short names).
const ECMA_GENERAL_CATEGORIES: &[&str] = &[
    "C",
    "Other",
    "Cc",
    "Control",
    "cntrl",
    "Cf",
    "Format",
    "Cn",
    "Unassigned",
    "Co",
    "Private_Use",
    "Cs",
    "Surrogate",
    "L",
    "Letter",
    "LC",
    "Cased_Letter",
    "Ll",
    "Lowercase_Letter",
    "Lm",
    "Modifier_Letter",
    "Lo",
    "Other_Letter",
    "Lt",
    "Titlecase_Letter",
    "Lu",
    "Uppercase_Letter",
    "M",
    "Mark",
    "Combining_Mark",
    "Mc",
    "Spacing_Mark",
    "Me",
    "Enclosing_Mark",
    "Mn",
    "Nonspacing_Mark",
    "N",
    "Number",
    "Nd",
    "Decimal_Number",
    "digit",
    "Nl",
    "Letter_Number",
    "No",
    "Other_Number",
    "P",
    "Punctuation",
    "punct",
    "Pc",
    "Connector_Punctuation",
    "Pd",
    "Dash_Punctuation",
    "Pe",
    "Close_Punctuation",
    "Pf",
    "Final_Punctuation",
    "Pi",
    "Initial_Punctuation",
    "Po",
    "Other_Punctuation",
    "Ps",
    "Open_Punctuation",
    "S",
    "Symbol",
    "Sc",
    "Currency_Symbol",
    "Sk",
    "Modifier_Symbol",
    "Sm",
    "Math_Symbol",
    "So",
    "Other_Symbol",
    "Z",
    "Separator",
    "Zl",
    "Line_Separator",
    "Zp",
    "Paragraph_Separator",
    "Zs",
    "Space_Separator",
];

/// ECMA-262 binary Unicode properties (long and short names).
const ECMA_BINARY_PROPERTIES: &[&str] = &[
    "ASCII",
    "ASCII_Hex_Digit",
    "AHex",
    "Alphabetic",
    "Alpha",
    "Any",
    "Assigned",
    "Bidi_Control",
    "Bidi_C",
    "Bidi_Mirrored",
    "Bidi_M",
    "Case_Ignorable",
    "CI",
    "Cased",
    "Changes_When_Casefolded",
    "CWCF",
    "Changes_When_Casemapped",
    "CWCM",
    "Changes_When_Lowercased",
    "CWL",
    "Changes_When_NFKC_Casefolded",
    "CWKCF",
    "Changes_When_Titlecased",
    "CWT",
    "Changes_When_Uppercased",
    "CWU",
    "Dash",
    "Default_Ignorable_Code_Point",
    "DI",
    "Deprecated",
    "Dep",
    "Diacritic",
    "Dia",
    "Emoji",
    "Emoji_Component",
    "EComp",
    "Emoji_Modifier",
    "EMod",
    "Emoji_Modifier_Base",
    "EBase",
    "Emoji_Presentation",
    "EPres",
    "Extended_Pictographic",
    "ExtPict",
    "Extender",
    "Ext",
    "Grapheme_Base",
    "Gr_Base",
    "Grapheme_Extend",
    "Gr_Ext",
    "Hex_Digit",
    "Hex",
    "IDS_Binary_Operator",
    "IDSB",
    "IDS_Trinary_Operator",
    "IDST",
    "ID_Continue",
    "IDC",
    "ID_Start",
    "IDS",
    "Ideographic",
    "Ideo",
    "Join_Control",
    "Join_C",
    "Logical_Order_Exception",
    "LOE",
    "Lowercase",
    "Lower",
    "Math",
    "Noncharacter_Code_Point",
    "NChar",
    "Pattern_Syntax",
    "Pat_Syn",
    "Pattern_White_Space",
    "Pat_WS",
    "Quotation_Mark",
    "QMark",
    "Radical",
    "Regional_Indicator",
    "RI",
    "Sentence_Terminal",
    "STerm",
    "Soft_Dotted",
    "SD",
    "Terminal_Punctuation",
    "Term",
    "Unified_Ideograph",
    "UIdeo",
    "Uppercase",
    "Upper",
    "Variation_Selector",
    "VS",
    "White_Space",
    "space",
    "XID_Continue",
    "XIDC",
    "XID_Start",
    "XIDS",
];

/// Whether `\p{name}` reads the same in ECMA-262's `u` mode. Rust matches
/// property names loosely (`\p{lu}`, `\p{sc:Greek}`) and accepts a lone
/// script name (`\p{Greek}`); ECMA-262 needs the exact spelling, and
/// `Script=` / `sc=` (or `Script_Extensions=` / `scx=`) for a script. So a
/// lone name must be a `General_Category` value or a binary property, and a
/// `key=value` needs a known key with a category value for `gc` or a
/// `Title_Case`-shaped script name.
fn is_ecma_property(name: &str) -> bool {
    match name.split_once('=') {
        None => ECMA_GENERAL_CATEGORIES.contains(&name) || ECMA_BINARY_PROPERTIES.contains(&name),
        Some(("General_Category" | "gc", value)) => ECMA_GENERAL_CATEGORIES.contains(&value),
        Some(("Script" | "sc" | "Script_Extensions" | "scx", value)) => {
            value.starts_with(|ch: char| ch.is_ascii_uppercase())
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphabetic() || ch == '_')
        }
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests;
