//! Parser for the field-level `#[schema(...)]` attribute constraints.
//!
//! Unlike the struct-level `#[schema(name=..., ref=..., nullable)]` parsers in
//! [`super::serde_attrs`], this module reads the *validation* keys that may
//! appear on individual fields:
//!
//! ```ignore
//! #[derive(vespera::Schema)]
//! pub struct CreateUser {
//!     #[schema(min_length = 3, max_length = 32, pattern = "^[a-z]+$")]
//!     pub username: String,
//!
//!     #[schema(minimum = 0, maximum = 150)]
//!     pub age: u32,
//!
//!     #[schema(format = "email")]
//!     pub email: String,
//!
//!     #[schema(read_only, example = "abc-123")]
//!     pub id: String,
//! }
//! ```
//!
//! The extracted [`SchemaConstraints`] flow into two different consumers:
//!
//! 1. **OpenAPI emission** (`struct_schema::parse_struct_to_schema`): the
//!    constraints are merged into the per-field `Schema` literal so that
//!    `openapi.json` exposes `minLength`, `maxLength`, `pattern`, … on the
//!    field schemas.
//! 2. **`garde::Validate` emission** (`schema_impl::process_derive_schema`,
//!    behind the `validation` feature): the same constraints are translated
//!    into `garde::rules::*::apply` calls inside the generated `validate_into`
//!    method body.
//!
//! Keys that have no garde counterpart (`example`, `read_only`, `write_only`,
//! `unique_items`) are still parsed — they only affect OpenAPI output.

use syn::{Attribute, Expr, ExprLit, Lit};

/// Field-level validation / documentation constraints carried by
/// `#[schema(...)]`.
///
/// Every field is `Option<_>` so an unset key means "no constraint".  The
/// shape mirrors the corresponding fields on
/// [`vespera_core::schema::Schema`] one-for-one — keep them in sync.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct SchemaConstraints {
    // ── string / array length ────────────────────────────────────────
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub pattern: Option<String>,

    // ── numeric range ────────────────────────────────────────────────
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub exclusive_minimum: Option<bool>,
    pub exclusive_maximum: Option<bool>,
    pub multiple_of: Option<f64>,

    // ── array constraints ────────────────────────────────────────────
    pub min_items: Option<usize>,
    pub max_items: Option<usize>,
    pub unique_items: Option<bool>,

    // ── OpenAPI annotations (no runtime validation) ──────────────────
    pub format: Option<String>,
    pub example: Option<serde_json::Value>,
    pub read_only: Option<bool>,
    pub write_only: Option<bool>,

    // ── nested validation ───────────────────────────────────────────
    /// When `Some(true)`, the field is recursively validated by
    /// invoking `garde::Validate::validate_into` on its value.  The
    /// path of any reported error is prefixed with the field name
    /// (e.g. `"address.city"`), and garde's runtime impls for
    /// `Option`, `Vec`, `HashMap`, `BTreeMap` automatically handle
    /// unwrapping / iteration.  This corresponds to
    /// `#[garde(dive)]` semantics and is opt-in to avoid accidental
    /// trait-bound failures on field types that don't implement
    /// `garde::Validate` (e.g. `chrono::DateTime`, `uuid::Uuid`,
    /// most third-party newtypes).
    pub dive: Option<bool>,
}

impl SchemaConstraints {
    /// `true` when no constraint keys were present on the field.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.min_length.is_none()
            && self.max_length.is_none()
            && self.pattern.is_none()
            && self.minimum.is_none()
            && self.maximum.is_none()
            && self.exclusive_minimum.is_none()
            && self.exclusive_maximum.is_none()
            && self.multiple_of.is_none()
            && self.min_items.is_none()
            && self.max_items.is_none()
            && self.unique_items.is_none()
            && self.format.is_none()
            && self.example.is_none()
            && self.read_only.is_none()
            && self.write_only.is_none()
            && self.dive.is_none()
    }

    /// `true` when at least one constraint produces a `garde` runtime rule
    /// (excludes pure-OpenAPI annotations such as `example` / `read_only` /
    /// `write_only` / `unique_items`).
    #[must_use]
    pub fn has_runtime_rule(&self) -> bool {
        self.min_length.is_some()
            || self.max_length.is_some()
            || self.pattern.is_some()
            || self.minimum.is_some()
            || self.maximum.is_some()
            || self.exclusive_minimum.is_some()
            || self.exclusive_maximum.is_some()
            || self.multiple_of.is_some()
            || self.min_items.is_some()
            || self.max_items.is_some()
            || matches!(
                self.format.as_deref(),
                Some("email" | "uri" | "url" | "ipv4" | "ipv6" | "ip")
            )
            || self.dive == Some(true)
    }
}

/// Extract all field-level `#[schema(...)]` validation / documentation
/// constraints from `attrs`.
///
/// Unknown keys are **silently ignored** so that struct-level keys
/// (`name`, `ref`, `nullable`) and future additions don't break this
/// parser when it walks a struct-level `#[schema(...)]` attribute.
#[must_use]
pub fn extract_schema_constraints(attrs: &[Attribute]) -> SchemaConstraints {
    let mut out = SchemaConstraints::default();
    for attr in attrs {
        if !attr.path().is_ident("schema") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            // ── string / array length ────────────────────────────────
            if meta.path.is_ident("min_length") {
                out.min_length = Some(parse_usize(&meta)?);
            } else if meta.path.is_ident("max_length") {
                out.max_length = Some(parse_usize(&meta)?);
            } else if meta.path.is_ident("pattern") {
                out.pattern = Some(parse_str(&meta)?);
            }
            // ── numeric range ────────────────────────────────────────
            else if meta.path.is_ident("minimum") {
                out.minimum = Some(parse_f64(&meta)?);
            } else if meta.path.is_ident("maximum") {
                out.maximum = Some(parse_f64(&meta)?);
            } else if meta.path.is_ident("exclusive_minimum") {
                out.exclusive_minimum = Some(parse_bool_or_default_true(&meta)?);
            } else if meta.path.is_ident("exclusive_maximum") {
                out.exclusive_maximum = Some(parse_bool_or_default_true(&meta)?);
            } else if meta.path.is_ident("multiple_of") {
                out.multiple_of = Some(parse_f64(&meta)?);
            }
            // ── array constraints ────────────────────────────────────
            else if meta.path.is_ident("min_items") {
                out.min_items = Some(parse_usize(&meta)?);
            } else if meta.path.is_ident("max_items") {
                out.max_items = Some(parse_usize(&meta)?);
            } else if meta.path.is_ident("unique_items") {
                out.unique_items = Some(parse_bool_or_default_true(&meta)?);
            }
            // ── OpenAPI annotations ──────────────────────────────────
            else if meta.path.is_ident("format") {
                out.format = Some(parse_str(&meta)?);
            } else if meta.path.is_ident("example") {
                out.example = Some(parse_example_value(&meta)?);
            } else if meta.path.is_ident("read_only") {
                out.read_only = Some(parse_bool_or_default_true(&meta)?);
            } else if meta.path.is_ident("write_only") {
                out.write_only = Some(parse_bool_or_default_true(&meta)?);
            } else if meta.path.is_ident("dive") {
                // Opt-in recursive validation.  Mirrors `#[garde(dive)]`:
                // emits a `Validate::validate_into` call so nested
                // structs, `Vec<Validate>`, `Option<Validate>`,
                // `HashMap<_, Validate>` are validated transparently
                // and errors carry dotted paths like "address.city".
                out.dive = Some(parse_bool_or_default_true(&meta)?);
            } else {
                // Unknown key — could be a struct-level key like `name`,
                // `ref`, `nullable`, `default` that lives on the same
                // `#[schema(...)]` attribute.  Consume any `= value`
                // payload so `parse_nested_meta` doesn't fail at the
                // trailing comma.
                if let Ok(value) = meta.value() {
                    let _: syn::Expr = value.parse()?;
                }
            }
            Ok(())
        });
    }
    out
}

// ── primitive value helpers ──────────────────────────────────────────

fn parse_usize(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<usize> {
    let lit: syn::LitInt = meta.value()?.parse()?;
    lit.base10_parse::<usize>()
}

fn parse_f64(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<f64> {
    let value = meta.value()?;
    let expr: Expr = value.parse()?;
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Float(f), ..
        }) => f.base10_parse::<f64>(),
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse::<f64>(),
        // Allow `minimum = -5` etc. — negation parses as a unary expression.
        Expr::Unary(unary) => {
            if let syn::UnOp::Neg(_) = unary.op
                && let Expr::Lit(ExprLit { lit, .. }) = *unary.expr
            {
                let positive = match lit {
                    Lit::Float(f) => f.base10_parse::<f64>()?,
                    Lit::Int(i) => i.base10_parse::<f64>()?,
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "expected a numeric literal after `-`",
                        ));
                    }
                };
                return Ok(-positive);
            }
            Err(syn::Error::new_spanned(unary, "expected a numeric literal"))
        }
        other => Err(syn::Error::new_spanned(
            other,
            "expected a numeric literal (int or float)",
        )),
    }
}

fn parse_str(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let lit: syn::LitStr = meta.value()?.parse()?;
    Ok(lit.value())
}

/// Parse a boolean attribute that may also appear as a bare keyword.
///
/// `#[schema(read_only)]`         → `true`
/// `#[schema(read_only = true)]`  → `true`
/// `#[schema(read_only = false)]` → `false`
fn parse_bool_or_default_true(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<bool> {
    // Try to parse a value; if there is no `=` after the key, fall back to
    // `true` (bare-keyword form).
    let Ok(value) = meta.value() else {
        return Ok(true);
    };
    let lit: syn::LitBool = value.parse()?;
    Ok(lit.value)
}

/// Parse an `example = ...` value into a `serde_json::Value`.
///
/// Accepts string / integer / float / boolean literals, and `null`.  More
/// complex shapes (objects, arrays) are not supported in attribute form —
/// users wanting structured examples should populate `example` programmatically
/// or via `#[schema(default = "...")]` which is already handled elsewhere.
fn parse_example_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<serde_json::Value> {
    let value = meta.value()?;
    let expr: Expr = value.parse()?;
    expr_to_json_value(&expr)
}

fn expr_to_json_value(expr: &Expr) -> syn::Result<serde_json::Value> {
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => lit_to_json_value(lit),
        Expr::Unary(unary) => {
            if let syn::UnOp::Neg(_) = unary.op
                && let Expr::Lit(ExprLit { lit, .. }) = unary.expr.as_ref()
            {
                let positive = lit_to_json_value(lit)?;
                // Try integer first so that `example = -5` round-trips
                // as `serde_json::json!(-5)` (i64) and not as the
                // semantically equal but type-distinct `-5.0` (f64).
                if let Some(i) = positive.as_i64() {
                    return Ok(serde_json::json!(-i));
                }
                if let Some(n) = positive.as_f64() {
                    return Ok(serde_json::json!(-n));
                }
            }
            Err(syn::Error::new_spanned(
                expr,
                "expected a literal after `-`",
            ))
        }
        Expr::Path(path) if path.path.is_ident("null") => Ok(serde_json::Value::Null),
        other => Err(syn::Error::new_spanned(
            other,
            "expected a literal value (string / int / float / bool / null)",
        )),
    }
}

fn lit_to_json_value(lit: &Lit) -> syn::Result<serde_json::Value> {
    match lit {
        Lit::Str(s) => Ok(serde_json::Value::String(s.value())),
        Lit::Bool(b) => Ok(serde_json::Value::Bool(b.value)),
        Lit::Int(i) => Ok(serde_json::json!(i.base10_parse::<i64>()?)),
        Lit::Float(f) => Ok(serde_json::json!(f.base10_parse::<f64>()?)),
        other => Err(syn::Error::new_spanned(
            other,
            "unsupported literal type for `example`",
        )),
    }
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn parse(attrs: &[Attribute]) -> SchemaConstraints {
        extract_schema_constraints(attrs)
    }

    #[test]
    fn empty_attrs_produce_empty_constraints() {
        let c = parse(&[]);
        assert!(c.is_empty());
        assert!(!c.has_runtime_rule());
    }

    #[test]
    fn unrelated_attrs_are_ignored() {
        let c = parse(&[parse_quote!(#[serde(rename = "x")])]);
        assert!(c.is_empty());
    }

    #[test]
    fn struct_level_keys_are_ignored() {
        // `name`, `ref`, `nullable`, `default` are handled by other parsers;
        // this parser must walk the same `#[schema(...)]` attribute without
        // tripping on them.
        let c = parse(&[parse_quote!(#[schema(name = "Foo", nullable)])]);
        assert!(c.is_empty());
    }

    #[test]
    fn min_max_length_int_literals() {
        let c = parse(&[parse_quote!(#[schema(min_length = 3, max_length = 64)])]);
        assert_eq!(c.min_length, Some(3));
        assert_eq!(c.max_length, Some(64));
        assert!(c.has_runtime_rule());
    }

    #[test]
    fn pattern_str_literal() {
        let c = parse(&[parse_quote!(#[schema(pattern = "^[a-z]+$")])]);
        assert_eq!(c.pattern.as_deref(), Some("^[a-z]+$"));
    }

    #[test]
    fn minimum_maximum_accept_both_int_and_float() {
        let c1 = parse(&[parse_quote!(#[schema(minimum = 0, maximum = 150)])]);
        assert_eq!(c1.minimum, Some(0.0));
        assert_eq!(c1.maximum, Some(150.0));
        let c2 = parse(&[parse_quote!(#[schema(minimum = 0.5, maximum = 99.9)])]);
        assert_eq!(c2.minimum, Some(0.5));
        assert_eq!(c2.maximum, Some(99.9));
    }

    #[test]
    fn negative_minimum() {
        let c = parse(&[parse_quote!(#[schema(minimum = -10)])]);
        assert_eq!(c.minimum, Some(-10.0));
    }

    #[test]
    fn exclusive_bounds_default_to_true_when_bare() {
        let c = parse(&[parse_quote!(#[schema(exclusive_minimum, exclusive_maximum)])]);
        assert_eq!(c.exclusive_minimum, Some(true));
        assert_eq!(c.exclusive_maximum, Some(true));
    }

    #[test]
    fn exclusive_bounds_explicit_false() {
        let c = parse(&[parse_quote!(#[schema(exclusive_minimum = false)])]);
        assert_eq!(c.exclusive_minimum, Some(false));
    }

    #[test]
    fn multiple_of_float() {
        let c = parse(&[parse_quote!(#[schema(multiple_of = 0.25)])]);
        assert_eq!(c.multiple_of, Some(0.25));
    }

    #[test]
    fn min_max_items_with_unique() {
        let c = parse(&[parse_quote!(#[schema(min_items = 1, max_items = 5, unique_items)])]);
        assert_eq!(c.min_items, Some(1));
        assert_eq!(c.max_items, Some(5));
        assert_eq!(c.unique_items, Some(true));
    }

    #[test]
    fn format_strings() {
        let c = parse(&[parse_quote!(#[schema(format = "email")])]);
        assert_eq!(c.format.as_deref(), Some("email"));
        assert!(c.has_runtime_rule());

        let c2 = parse(&[parse_quote!(#[schema(format = "uuid")])]);
        // uuid has no garde rule — annotation only, no runtime rule.
        assert!(!c2.has_runtime_rule());
    }

    #[test]
    fn example_with_various_literal_kinds() {
        let c = parse(&[parse_quote!(#[schema(example = "hello")])]);
        assert_eq!(c.example, Some(serde_json::json!("hello")));

        let c = parse(&[parse_quote!(#[schema(example = 42)])]);
        assert_eq!(c.example, Some(serde_json::json!(42)));

        let c = parse(&[parse_quote!(#[schema(example = 2.5)])]);
        assert_eq!(c.example, Some(serde_json::json!(2.5)));

        let c = parse(&[parse_quote!(#[schema(example = true)])]);
        assert_eq!(c.example, Some(serde_json::json!(true)));

        let c = parse(&[parse_quote!(#[schema(example = -5)])]);
        assert_eq!(c.example, Some(serde_json::json!(-5)));
    }

    #[test]
    fn read_only_write_only_bare_and_explicit() {
        let c = parse(&[parse_quote!(#[schema(read_only, write_only = false)])]);
        assert_eq!(c.read_only, Some(true));
        assert_eq!(c.write_only, Some(false));
    }

    #[test]
    fn dive_bare_keyword_defaults_to_true() {
        let c = parse(&[parse_quote!(#[schema(dive)])]);
        assert_eq!(c.dive, Some(true));
        assert!(c.has_runtime_rule(), "dive should count as a runtime rule");
    }

    #[test]
    fn dive_explicit_false_disables_runtime_rule() {
        let c = parse(&[parse_quote!(#[schema(dive = false)])]);
        assert_eq!(c.dive, Some(false));
        assert!(
            !c.has_runtime_rule(),
            "dive = false must not register a runtime rule"
        );
    }

    #[test]
    fn dive_combines_with_other_constraints() {
        let c = parse(&[parse_quote!(#[schema(min_items = 1, max_items = 10, dive)])]);
        assert_eq!(c.min_items, Some(1));
        assert_eq!(c.max_items, Some(10));
        assert_eq!(c.dive, Some(true));
    }

    #[test]
    fn mixed_struct_and_field_keys_in_one_attr_are_partitioned_correctly() {
        // A user might write a single `#[schema(name = "...", min_length = 3)]`.
        // The struct-level `name` is ignored here; the field-level
        // `min_length` is parsed.
        let c = parse(&[parse_quote!(#[schema(name = "MyType", min_length = 3)])]);
        assert!(c.name_unaffected());
        assert_eq!(c.min_length, Some(3));
    }

    #[test]
    fn multiple_schema_attrs_accumulate() {
        let attrs: [Attribute; 2] = [
            parse_quote!(#[schema(min_length = 3)]),
            parse_quote!(#[schema(max_length = 32, format = "email")]),
        ];
        let c = parse(&attrs);
        assert_eq!(c.min_length, Some(3));
        assert_eq!(c.max_length, Some(32));
        assert_eq!(c.format.as_deref(), Some("email"));
    }

    impl SchemaConstraints {
        // helper for the partitioning test above — kept private to the
        // tests module so it doesn't pollute the public surface.
        fn name_unaffected(&self) -> bool {
            self.format.is_none() && self.example.is_none() && self.read_only.is_none()
        }
    }
}
