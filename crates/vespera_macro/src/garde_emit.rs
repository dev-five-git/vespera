//! Code generation for `impl ::vespera::__validation::garde::Validate`.
//!
//! When the `validation` feature is enabled on `vespera_macro`,
//! `#[derive(Schema)]` calls [`emit_garde_validate`] to produce a
//! token stream containing the `Validate` trait implementation.  The
//! generated code references garde indirectly via the facade module
//! `::vespera::__validation::garde::...` so user crates never need to
//! depend on `garde` directly.
//!
//! ## Limitations (v1)
//!
//! - **Enums**: no `Validate` impl is emitted.
//! - **Generic / lifetime-parameterised structs**: if the struct
//!   carries any constraints and also any generic parameter, the macro
//!   emits a `compile_error!` rather than guessing at trait bounds.
//! - **Tuple / unit structs**: no `Validate` impl is emitted.
//! - **`format = "uuid"`**: produces an OpenAPI annotation only; garde
//!   has no built-in UUID validator, and we don't synthesise one.
//! - **`exclusive_minimum` / `exclusive_maximum`**: OpenAPI annotation
//!   only; garde's `range` rule is inclusive on both sides.
//! - **`multiple_of`**: OpenAPI annotation only; no garde counterpart.
//! - **`unique_items`**: OpenAPI annotation only.

// `TokenStream` and `DeriveInput` are used by both the `validation`-on
// and `validation`-off stubs of `emit_garde_validate` below, so they
// stay outside any `cfg` gate.  Everything else is only referenced from
// the `#[cfg(feature = "validation")]` code path and would trip
// `-D unused-imports` when this crate is built with default features
// (e.g. during `cargo publish --dry-run`).
use proc_macro2::TokenStream;
use syn::DeriveInput;

// `SchemaConstraints` appears in the public signature of
// `emit_garde_validate`, so it must be in scope in BOTH the
// `validation`-on and `validation`-off arms — the off-arm signature still
// names the slice type even though it ignores the argument.
use crate::parser::schema::schema_attrs::SchemaConstraints;

#[cfg(feature = "validation")]
use proc_macro2::Span;
#[cfg(feature = "validation")]
use quote::{format_ident, quote};
#[cfg(feature = "validation")]
use syn::{Data, Fields, Type};

/// Public entry point used by `process_derive_schema`.
///
/// When `validation` is **off** on `vespera_macro`, this expands to an
/// empty stub via the `#[cfg(...)]` switch at the bottom of this file.
///
/// `field_constraints` is the pre-parsed `#[schema(...)]` slice already
/// produced by `process_derive_schema` (one entry per named field, in
/// declaration order). Threading it through here removes the third walk
/// over every field's `attrs` the previous code performed inside
/// `emit_impl`.
#[cfg(feature = "validation")]
#[must_use]
pub fn emit_garde_validate(
    input: &DeriveInput,
    field_constraints: &[SchemaConstraints],
) -> TokenStream {
    emit_impl(input, field_constraints)
}

#[cfg(not(feature = "validation"))]
#[must_use]
pub fn emit_garde_validate(
    _input: &DeriveInput,
    _field_constraints: &[SchemaConstraints],
) -> TokenStream {
    TokenStream::new()
}

#[cfg(feature = "validation")]
fn emit_impl(input: &DeriveInput, field_constraints: &[SchemaConstraints]) -> TokenStream {
    // Only structs with named fields are validated; everything else
    // produces an empty token stream so the derive remains a no-op.
    let Data::Struct(data_struct) = &input.data else {
        return TokenStream::new();
    };
    let Fields::Named(fields_named) = &data_struct.fields else {
        return TokenStream::new();
    };

    // `process_derive_schema` parsed every field's `#[schema(...)]` once and
    // bailed early on the `Err` arm via `to_compile_error`, so by the time we
    // reach this function we already have a slice with one valid entry per
    // named field. Pairwise zip recovers the `(&Field, &SchemaConstraints)`
    // view the previous code reproduced by re-parsing the attrs.
    let per_field: Vec<(&syn::Field, &SchemaConstraints)> =
        fields_named.named.iter().zip(field_constraints).collect();

    // A rule on a serde-skipped field is never checked (see `field_blocks`), but
    // the impl is still emitted when such a rule exists: its body is empty (the
    // skipped field's bindings are `allow(unused_variables)`), and that impl is
    // what lets `Validated<Json<S>>` accept the struct. A struct with no runtime
    // rule at all gets no impl (below), so `Validated` rejects it at compile
    // time (`crates/vespera/tests/ui/validated_payload_without_validate.rs`).
    if per_field.iter().all(|(_, c)| !c.has_runtime_rule()) {
        // No field requested a runtime rule — skip Validate emission.
        // OpenAPI annotation-only constraints (example / read_only /
        // write_only / unique_items / exclusive bounds / multiple_of /
        // format=uuid) still made it into the schema via the OpenAPI
        // path; they just don't need a garde impl.
        return TokenStream::new();
    }

    // Bail with a clear compile error for generic types — supporting
    // them properly would require synthesising `where` bounds based on
    // which generic parameters appear in validated field types.  Out
    // of scope for v1.
    if !input.generics.params.is_empty() {
        let msg = format!(
            "vespera::Schema validation does not yet support generic / \
             lifetime-parameterised types (struct `{}`).  Move the \
             `#[schema(...)]` constraints to a non-generic wrapper, or \
             open an issue if you need this.",
            input.ident,
        );
        return quote! { ::std::compile_error!(#msg); };
    }

    let struct_ident = &input.ident;
    let field_idents: Vec<&syn::Ident> = fields_named
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref())
        .collect();

    // Error paths name the field as the CLIENT sent it: serde's deserialize
    // side (`rename(deserialize = ..)` / `rename_all`, raw `r#` stripped) —
    // the same resolution the request-view schema documents.
    let rename_all =
        crate::parser::extract_rename_all_for(&input.attrs, crate::parser::SerdeSide::Deserialize);
    // A `#[serde(transparent)]` struct IS its one field on the wire: like a
    // flattened field, its errors keep the parent's path (`""`, or
    // `contact` — not `contact.value`).
    let transparent = crate::parser::schema::extract_transparent(&input.attrs);
    let field_blocks: Vec<TokenStream> = per_field
        .iter()
        .filter_map(|(field, constraints)| {
            let ident = field.ident.as_ref()?;
            // serde never reads a skipped field (it holds its default): a
            // rule on it would reject every request.
            let skip = crate::parser::schema::extract_serde_skip_flags(&field.attrs);
            if skip.skip || skip.skip_deserializing {
                return None;
            }
            let wire_name = crate::parser::extract_field_rename_for(
                &field.attrs,
                crate::parser::SerdeSide::Deserialize,
            )
            .unwrap_or_else(|| {
                crate::parser::rename_field(
                    &crate::parser::strip_raw_prefix_owned(ident.to_string()),
                    rename_all.as_deref(),
                )
            });
            // A flattened field's keys sit at the parent's level on the
            // wire, so its errors keep the parent's path.
            let flatten = transparent || crate::parser::schema::extract_flatten(&field.attrs);
            emit_field_block(ident, (&wire_name, flatten), field, constraints)
        })
        .collect();
    // Non-empty: the gate above found a field with a runtime rule that serde
    // reads, and every such (named) field emits a block.

    quote! {
        #[allow(
            clippy::all,
            clippy::pedantic,
            clippy::nursery,
            unused_variables,
            unused_mut,
            unused_parens,
            non_upper_case_globals,
        )]
        impl ::vespera::__validation::garde::Validate for #struct_ident {
            type Context = ();

            fn validate_into(
                &self,
                __garde_user_ctx: &Self::Context,
                mut __garde_path: &mut dyn ::core::ops::FnMut() -> ::vespera::__validation::garde::Path,
                __garde_report: &mut ::vespera::__validation::garde::Report,
            ) {
                let _ = __garde_user_ctx; // suppress unused warning when no `custom` rules
                let Self { #(#field_idents),* } = self;
                #(#field_blocks)*
            }
        }
    }
}

#[cfg(feature = "validation")]
fn emit_field_block(
    field_ident: &syn::Ident,
    (field_name_str, flatten): (&str, bool),
    field: &syn::Field,
    c: &SchemaConstraints,
) -> Option<TokenStream> {
    if !c.has_runtime_rule() {
        return None;
    }

    let field_ty = &field.ty;
    // Every `Option` layer: `Option<Option<i32>>` still ranges as an `i32`.
    let mut numeric_ty = field_ty;
    let mut option_layers = 0_usize;
    while let Some(inner) = crate::schema_macro::type_utils::option_inner(numeric_ty) {
        numeric_ty = inner;
        option_layers += 1;
    }
    let numeric_kind = rust_numeric_kind(numeric_ty);
    let exact = ExactBounds::from_attrs(&field.attrs);
    let rule_blocks = emit_rule_blocks(c, &exact, field_ident, numeric_kind.as_deref());
    // `has_runtime_rule` holds, so at least one of the two is non-empty.
    let dive_block = emit_dive_block(c);

    // The field's own path segment; a flattened field keeps the parent's.
    let nest = if flatten {
        TokenStream::new()
    } else {
        quote! { let mut __garde_path = ::vespera::__validation::garde::util::nested_path!(__garde_path, #field_name_str); }
    };
    let block = if crate::schema_macro::type_utils::is_option_type(field_ty) {
        // `field_ident` is `&Option<T>` after the `let Self { .. } = self` destructure.
        // Match ergonomics make `inner` end up as `&T`. Each further layer is
        // peeled the same way, so the rules see the innermost value.
        let mut checked = quote! {
            #rule_blocks
            #dive_block
        };
        for _ in 1..option_layers {
            checked = quote! {
                if let ::std::option::Option::Some(__garde_binding) = __garde_binding {
                    #checked
                }
            };
        }
        quote! {
            {
                #nest
                if let ::std::option::Option::Some(__garde_binding) = #field_ident {
                    #checked
                }
            }
        }
    } else {
        quote! {
            {
                #nest
                let __garde_binding = &*#field_ident;
                #rule_blocks
                #dive_block
            }
        }
    };

    Some(block)
}

/// Emit the `garde::Validate::validate_into` call for fields annotated
/// with `#[schema(dive)]`.
///
/// Garde's runtime `Validate` impls for `Option<T>`, `Vec<T>`,
/// `HashMap<K, V>`, and `BTreeMap<K, V>` automatically unwrap /
/// iterate, so the emitted call is identical regardless of container —
/// it dispatches to the appropriate impl by trait resolution and the
/// runtime pushes the right path components (`name`, `tags[0]`,
/// `m["key"]`, …).
///
/// For Option-typed fields we already emit an outer `if let Some(...)`
/// so `__garde_binding` is the unwrapped inner value here; the
/// `Option`-aware behaviour is therefore intentionally bypassed for
/// uniformity with the other rule blocks in this file.
#[cfg(feature = "validation")]
fn emit_dive_block(c: &SchemaConstraints) -> TokenStream {
    if c.dive == Some(true) {
        quote! {
            ::vespera::__validation::garde::Validate::validate_into(
                &*__garde_binding,
                __garde_user_ctx,
                &mut __garde_path,
                __garde_report,
            );
        }
    } else {
        TokenStream::new()
    }
}

#[cfg(feature = "validation")]
#[allow(clippy::too_many_lines)] // exhaustive rule-to-emit dispatcher
fn emit_rule_blocks(
    c: &SchemaConstraints,
    exact: &ExactBounds,
    field_ident: &syn::Ident,
    numeric_kind: Option<&str>,
) -> TokenStream {
    let field_name = &field_ident.to_string();
    let mut blocks: Vec<TokenStream> = Vec::new();

    // ── String length (min_length / max_length → length::chars) ───────
    if c.min_length.is_some() || c.max_length.is_some() {
        let min = c.min_length.unwrap_or(0);
        let max = usize_bound(c.max_length);
        blocks.push(quote! {
            if let ::std::result::Result::Err(__garde_error) =
                (::vespera::__validation::garde::rules::length::chars::apply)(
                    &*__garde_binding,
                    (#min, #max),
                )
            {
                __garde_report.append(__garde_path(), __garde_error);
            }
        });
    }

    // ── Array length (min_items / max_items → length::simple) ─────────
    if c.min_items.is_some() || c.max_items.is_some() {
        let min = c.min_items.unwrap_or(0);
        let max = usize_bound(c.max_items);
        blocks.push(quote! {
            if let ::std::result::Result::Err(__garde_error) =
                (::vespera::__validation::garde::rules::length::simple::apply)(
                    &*__garde_binding,
                    (#min, #max),
                )
            {
                __garde_report.append(__garde_path(), __garde_error);
            }
        });
    }

    // ── Numeric range (minimum / maximum → range::apply) ──────────────
    if c.minimum.is_some() || c.maximum.is_some() {
        let bounds =
            numeric_some(c.minimum, exact.minimum, Bound::Min, numeric_kind).and_then(|min| {
                numeric_some(c.maximum, exact.maximum, Bound::Max, numeric_kind)
                    .map(|max| (min, max))
            });
        let (min_expr, max_expr) = match bounds {
            Ok(bounds) => bounds,
            Err(msg) => {
                let msg = format!("vespera: `#[schema(...)]` on field `{field_name}`: {msg}");
                blocks.push(syn::Error::new(field_ident.span(), msg).to_compile_error());
                return quote! { #(#blocks)* };
            }
        };
        let range = quote! {
            if let ::std::result::Result::Err(__garde_error) =
                (::vespera::__validation::garde::rules::range::apply)(
                    __garde_binding,
                    (#min_expr, #max_expr),
                )
            {
                __garde_report.append(__garde_path(), __garde_error);
            }
        };
        // An aliased field's fractional bound saturates into its type's range
        // (`minimum = 300.5` on `type Small = u8` would admit `255`); reject a
        // bound no value of the type reaches before the range rule runs.
        let fractional = |value: Option<f64>, exact: Option<ExactInt>| match value {
            Some(v) if numeric_kind.is_none() && exact.is_none() => {
                quote!(::std::option::Option::Some(#v))
            }
            _ => quote!(::std::option::Option::None),
        };
        let unreachable_min = fractional(c.minimum, exact.minimum);
        let unreachable_max = fractional(c.maximum, exact.maximum);
        let has_fractional = numeric_kind.is_none()
            && (exact.minimum.is_none() && c.minimum.is_some()
                || exact.maximum.is_none() && c.maximum.is_some());
        let guard = if has_fractional {
            quote! { if let ::std::option::Option::Some(__garde_error) = ::vespera::__validation::range_unsatisfiable(__garde_binding, #unreachable_min, #unreachable_max) { __garde_report.append(__garde_path(), __garde_error); } else }
        } else {
            TokenStream::new()
        };
        // `garde`'s range rule admits `NaN` (every comparison with it is
        // false); a float field, or one whose type the macro cannot see
        // through, rejects a non-finite value first.
        let finite = if matches!(numeric_kind, None | Some("f32" | "f64")) {
            quote! { if let ::std::option::Option::Some(__garde_error) = ::vespera::__validation::non_finite(__garde_binding) { __garde_report.append(__garde_path(), __garde_error); } else }
        } else {
            TokenStream::new()
        };
        blocks.push(quote! { #finite #guard #range });
    }

    // ── Pattern (pattern = "..." → static LazyLock<Regex>) ────────────
    if let Some(pattern) = &c.pattern {
        // Validate the user-supplied regex at MACRO-EXPANSION time with
        // `regex-syntax` (the exact parser `regex` uses), so an invalid
        // pattern becomes a COMPILE error naming the field instead of a
        // first-validation runtime panic. Only a syntactically valid pattern
        // reaches codegen; the runtime `Regex::new` fallback below is retained
        // solely for the rare case a valid pattern exceeds `regex`'s compiled
        // size limit (which `regex-syntax` parsing does not enforce).
        if let Err(__err) = regex_syntax::Parser::new().parse(pattern) {
            let msg = format!(
                "vespera: `#[schema(pattern = {pattern:?})]` on field `{field_name}` is not a valid regex: {__err}"
            );
            blocks.push(quote! { ::std::compile_error!(#msg); });
        } else {
            // Sanitize the field name into a valid identifier fragment before
            // splicing it into a `static` name: strip a raw-identifier `r#`
            // prefix and map any non-alphanumeric byte to `_`.  A raw ident
            // (`r#type`) or otherwise unusual field name would otherwise make
            // `format_ident!` PANIC at macro-expansion time (e.g.
            // `__VESPERA_PATTERN_R#TYPE` is not a valid ident).  Each pattern
            // block is emitted in its own `{ }` scope, so the sanitized name
            // never needs to be unique across fields.
            let ident_fragment: String = field_name
                .trim_start_matches("r#")
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            let static_ident = format_ident!("__VESPERA_PATTERN_{}", ident_fragment);
            blocks.push(quote! {
                {
                    static #static_ident: ::std::sync::LazyLock<
                        ::vespera::__validation::garde::rules::pattern::regex::Regex,
                    > = ::std::sync::LazyLock::new(|| {
                        // Pattern syntax was validated at macro expansion; this
                        // fallback only trips on the rare compiled-size-limit
                        // case, with an actionable message naming the pattern.
                        ::vespera::__validation::garde::rules::pattern::regex::Regex::new(#pattern)
                            .unwrap_or_else(|__e| {
                                ::std::panic!(
                                    "vespera: `#[schema(pattern = {:?})]` is not a valid regex: {__e}",
                                    #pattern
                                )
                            })
                    });
                    if let ::std::result::Result::Err(__garde_error) =
                        (::vespera::__validation::garde::rules::pattern::apply)(
                            &*__garde_binding,
                            (&*#static_ident,),
                        )
                    {
                        __garde_report.append(__garde_path(), __garde_error);
                    }
                }
            });
        }
    }

    // ── Format-driven rules (email / uri / ipv4 / ipv6 / ip) ──────────
    if let Some(fmt) = c.format.as_deref() {
        match fmt {
            "email" => blocks.push(quote! {
                if let ::std::result::Result::Err(__garde_error) =
                    (::vespera::__validation::garde::rules::email::apply)(
                        &*__garde_binding,
                        (),
                    )
                {
                    __garde_report.append(__garde_path(), __garde_error);
                }
            }),
            "uri" | "url" => blocks.push(quote! {
                if let ::std::result::Result::Err(__garde_error) =
                    (::vespera::__validation::garde::rules::url::apply)(
                        &*__garde_binding,
                        (),
                    )
                {
                    __garde_report.append(__garde_path(), __garde_error);
                }
            }),
            "ipv4" => blocks.push(quote! {
                if let ::std::result::Result::Err(__garde_error) =
                    (::vespera::__validation::garde::rules::ip::apply)(
                        &*__garde_binding,
                        (::vespera::__validation::garde::rules::ip::IpKind::V4,),
                    )
                {
                    __garde_report.append(__garde_path(), __garde_error);
                }
            }),
            "ipv6" => blocks.push(quote! {
                if let ::std::result::Result::Err(__garde_error) =
                    (::vespera::__validation::garde::rules::ip::apply)(
                        &*__garde_binding,
                        (::vespera::__validation::garde::rules::ip::IpKind::V6,),
                    )
                {
                    __garde_report.append(__garde_path(), __garde_error);
                }
            }),
            "ip" => blocks.push(quote! {
                if let ::std::result::Result::Err(__garde_error) =
                    (::vespera::__validation::garde::rules::ip::apply)(
                        &*__garde_binding,
                        (::vespera::__validation::garde::rules::ip::IpKind::Any,),
                    )
                {
                    __garde_report.append(__garde_path(), __garde_error);
                }
            }),
            // "uuid" / "date" / "date-time" / "byte" / "binary" /
            // "password" / "hostname" / "regex" → OpenAPI annotation
            // only; no garde counterpart.  Silently skip.
            _ => {}
        }
    }

    quote! { #(#blocks)* }
}

// ── helpers ──────────────────────────────────────────────────────────

/// The `usize` upper bound of a length rule: the declared value, or the
/// TARGET's `usize::MAX` — never the host's value baked in as a literal
/// (`18446744073709551615usize` does not compile for 32-bit targets).
#[cfg(feature = "validation")]
fn usize_bound(value: Option<usize>) -> TokenStream {
    value.map_or_else(
        || quote! { ::core::primitive::usize::MAX },
        |max| quote! { #max },
    )
}

/// An integer-literal `#[schema(minimum / maximum = ...)]` bound exactly as
/// written (`-0` is non-negative).
#[cfg(feature = "validation")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactInt {
    negative: bool,
    magnitude: u128,
}

#[cfg(feature = "validation")]
impl std::fmt::Display for ExactInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.negative { "-" } else { "" };
        write!(f, "{sign}{}", self.magnitude)
    }
}

#[cfg(feature = "validation")]
impl ExactInt {
    /// `5` / `-5` (an integer literal, optionally negated); `None` for
    /// floats and anything else.
    fn from_expr(expr: &syn::Expr) -> Option<Self> {
        let (negative, expr) = match expr {
            syn::Expr::Unary(syn::ExprUnary {
                op: syn::UnOp::Neg(_),
                expr,
                ..
            }) => (true, &**expr),
            expr => (false, expr),
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit),
            ..
        }) = expr
        else {
            return None;
        };
        // A trailing `key = -7` reaches us as ONE negative literal (syn's
        // attribute value parser reads `-7` as a `Lit`), not as a negation.
        let digits = lit.base10_digits();
        let (negative_literal, digits) = digits
            .strip_prefix('-')
            .map_or((false, digits), |digits| (true, digits));
        let magnitude = digits.parse::<u128>().ok()?;
        Some(Self {
            negative: negative != negative_literal && magnitude != 0,
            magnitude,
        })
    }
}

/// The integer-literal `minimum` / `maximum` of a field's `#[schema(...)]`
/// attributes (the parsed [`SchemaConstraints`] only keeps an `f64` copy).
#[cfg(feature = "validation")]
#[derive(Debug, Default, PartialEq, Eq)]
struct ExactBounds {
    minimum: Option<ExactInt>,
    maximum: Option<ExactInt>,
}

#[cfg(feature = "validation")]
impl ExactBounds {
    /// Walks the attributes exactly like `try_extract_schema_constraints`
    /// (every key's payload consumed, a `value_type` read as a type, the
    /// last `minimum` / `maximum` winning), so the exact bound always
    /// belongs to the `f64` the parsed constraints carry.
    fn from_attrs(attrs: &[syn::Attribute]) -> Self {
        let mut bounds = Self::default();
        for attr in attrs.iter().filter(|attr| attr.path().is_ident("schema")) {
            // The derive already rejected a malformed attribute.
            let _ = attr.parse_nested_meta(|meta| {
                let slot = if meta.path.is_ident("minimum") {
                    &mut bounds.minimum
                } else if meta.path.is_ident("maximum") {
                    &mut bounds.maximum
                } else {
                    if meta.path.is_ident("value_type") {
                        meta.value()?.parse::<syn::Type>()?;
                    } else if meta.input.peek(syn::token::Paren) {
                        meta.input.parse::<proc_macro2::Group>()?;
                    } else if let Ok(value) = meta.value() {
                        value.parse::<syn::Expr>()?;
                    }
                    return Ok(());
                };
                *slot = ExactInt::from_expr(&meta.value()?.parse()?);
                Ok(())
            });
        }
        bounds
    }

    /// The looser of the exact bound and the documented (`f64`) one `v`,
    /// within the type's range: past 2^53 the OpenAPI document can only
    /// carry a rounded value, and a client sending that advertised bound
    /// must not be rejected.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn loosen(
        exact: ExactInt,
        v: f64,
        bound: Bound,
        (type_min, type_max): (i128, u128),
    ) -> ExactInt {
        // `as` saturates: an out-of-range `f64` lands at the `u128` edge and
        // is clamped below.
        let doc = ExactInt {
            negative: v < 0.0,
            magnitude: v.abs() as u128,
        };
        let looser = match bound {
            Bound::Min => exact.min(doc),
            Bound::Max => exact.max(doc),
        };
        let lowest = ExactInt {
            negative: type_min < 0,
            magnitude: type_min.unsigned_abs(),
        };
        let highest = ExactInt {
            negative: false,
            magnitude: type_max,
        };
        looser.clamp(lowest, highest)
    }
}

#[cfg(feature = "validation")]
impl Ord for ExactInt {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.negative, other.negative) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => self.magnitude.cmp(&other.magnitude),
            (true, true) => other.magnitude.cmp(&self.magnitude),
        }
    }
}

#[cfg(feature = "validation")]
impl PartialOrd for ExactInt {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Which side of a `range` rule a bound sits on: an integer field rounds a
/// fractional bound INWARD (`minimum` up, `maximum` down), so the runtime rule
/// accepts exactly the integers the declared real interval contains.
#[cfg(feature = "validation")]
#[derive(Clone, Copy)]
enum Bound {
    Min,
    Max,
}

/// Inclusive value range of a Rust integer type, as `(min, max)`.
/// `isize` / `usize` use the 64-bit range (the host cannot know the target's
/// pointer width; a too-large literal still fails the target's own compile).
#[cfg(feature = "validation")]
const fn integer_range(kind: &str) -> Option<(i128, u128)> {
    Some(match kind.as_bytes() {
        b"i8" => (i8::MIN as i128, i8::MAX as u128),
        b"i16" => (i16::MIN as i128, i16::MAX as u128),
        b"i32" => (i32::MIN as i128, i32::MAX as u128),
        b"i64" | b"isize" => (i64::MIN as i128, i64::MAX as u128),
        b"i128" => (i128::MIN, i128::MAX as u128),
        b"u8" => (0, u8::MAX as u128),
        b"u16" => (0, u16::MAX as u128),
        b"u32" => (0, u32::MAX as u128),
        b"u64" | b"usize" => (0, u64::MAX as u128),
        b"u128" => (0, u128::MAX),
        _ => return None,
    })
}

/// Render one `range` bound in the field's own type.
///
/// Integer fields get a range-checked, suffixed integer literal (a bound the
/// type cannot hold is a compile error rather than a wrapping `as` cast);
/// float fields keep the declared value; unknown types (an alias) convert
/// every bound through `vespera::__validation::RangeBound`, which rounds a
/// fractional bound inward for an integer target and keeps it for a float
/// one, and takes an integer literal in the target's own type.
#[cfg(feature = "validation")]
fn numeric_some(
    value: Option<f64>,
    exact: Option<ExactInt>,
    bound: Bound,
    numeric_kind: Option<&str>,
) -> Result<TokenStream, String> {
    let Some(v) = value else {
        return Ok(quote! { ::std::option::Option::None });
    };
    let key = match bound {
        Bound::Min => "minimum",
        Bound::Max => "maximum",
    };
    // `v` is finite: `schema_attrs` rejects a non-finite bound at parse time.
    let Some(kind) = numeric_kind else {
        // Unknown numeric kind (a type alias, a newtype): every bound goes
        // through `RangeBound`, typed by the field. An integer literal is
        // passed as `RangeBound::Literal` (the integer type itself, so the
        // compiler still range-checks it; `i128` for a float, which a bare
        // unsuffixed `0` never infers as); a fractional one is rounded inward
        // for an integer target (an `as _` cast truncated `minimum = 0.5` on
        // `type Count = u32` to 0). A non-numeric field type is a compiler
        // error at the field.
        if let Some(exact) = exact {
            // The documented `f64` travels with the literal, so the validator
            // loosens to it exactly like a known integer type does.
            let literal: TokenStream = exact
                .to_string()
                .parse()
                .expect("an integer literal is a valid token stream");
            let lower = matches!(bound, Bound::Min);
            return Ok(
                quote! { ::std::option::Option::Some(<_ as ::vespera::__validation::RangeBound>::exact_within(#literal, #v, #lower)) },
            );
        }
        let convert = match bound {
            Bound::Min => quote!(min),
            Bound::Max => quote!(max),
        };
        return Ok(
            quote! { ::std::option::Option::Some(<_ as ::vespera::__validation::RangeBound>::#convert(#v)) },
        );
    };
    let ty_ident = syn::Ident::new(kind, Span::call_site());
    let Some((type_min, type_max)) = integer_range(kind) else {
        return Ok(quote! { ::std::option::Option::Some(#v as #ty_ident) });
    };
    if let Some(exact) = exact {
        // An integer literal bound keeps every digit: the `f64` copy loses
        // precision past 2^53 (`i64::MAX` would read as 2^63 and "not fit").
        let fits = if exact.negative {
            exact.magnitude <= type_min.unsigned_abs()
        } else {
            exact.magnitude <= type_max
        };
        if !fits {
            return Err(format!(
                "`{key} = {exact}` does not fit the field type `{kind}` (range {type_min}..={type_max})"
            ));
        }
        let exact = ExactBounds::loosen(exact, v, bound, (type_min, type_max));
        let literal: TokenStream = format!("{exact}{kind}")
            .parse()
            .expect("an integer literal with a type suffix is a valid token stream");
        return Ok(quote! { ::std::option::Option::Some(#literal) });
    }
    // `+ 0.0` turns a `-0.0` (`minimum = -0.5` rounded up) into `0.0`, so no
    // `-0u32` literal is ever emitted.
    let rounded = match bound {
        Bound::Min => v.ceil() + 0.0,
        Bound::Max => v.floor() + 0.0,
    };
    // Every integer type's MIN is a power of two (exact as f64), so the lower
    // bound compares in the float domain; the upper one compares as `u128`
    // (`as` saturates, hence the explicit 2^128 guard).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let fits = rounded < 340_282_366_920_938_463_463_374_607_431_768_211_456.0
        && rounded >= type_min as f64
        && (rounded < 0.0 || rounded as u128 <= type_max);
    if !fits {
        return Err(format!(
            "`{key} = {v}` does not fit the field type `{kind}` (range {type_min}..={type_max})"
        ));
    }
    let literal: TokenStream = format!("{rounded:.0}{kind}")
        .parse()
        .expect("an integer literal with a type suffix is a valid token stream");
    Ok(quote! { ::std::option::Option::Some(#literal) })
}

#[cfg(feature = "validation")]
fn rust_numeric_kind(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let last = tp.path.segments.last()?;
    let name = last.ident.to_string();
    matches!(
        name.as_str(),
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    )
    .then_some(name)
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "validation"))]
mod tests;
#[cfg(all(test, feature = "validation"))]
mod tests_wire;
