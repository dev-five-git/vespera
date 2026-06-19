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

#[cfg(feature = "validation")]
use proc_macro2::Span;
#[cfg(feature = "validation")]
use quote::{format_ident, quote};
#[cfg(feature = "validation")]
use syn::{Data, Fields, Type};

#[cfg(feature = "validation")]
use crate::parser::schema::schema_attrs::{SchemaConstraints, extract_schema_constraints};

/// Public entry point used by `process_derive_schema`.
///
/// When `validation` is **off** on `vespera_macro`, this expands to an
/// empty stub via the `#[cfg(...)]` switch at the bottom of this file.
#[cfg(feature = "validation")]
#[must_use]
pub fn emit_garde_validate(input: &DeriveInput) -> TokenStream {
    emit_impl(input)
}

#[cfg(not(feature = "validation"))]
#[must_use]
pub fn emit_garde_validate(_input: &DeriveInput) -> TokenStream {
    TokenStream::new()
}

#[cfg(feature = "validation")]
fn emit_impl(input: &DeriveInput) -> TokenStream {
    // Only structs with named fields are validated; everything else
    // produces an empty token stream so the derive remains a no-op.
    let Data::Struct(data_struct) = &input.data else {
        return TokenStream::new();
    };
    let Fields::Named(fields_named) = &data_struct.fields else {
        return TokenStream::new();
    };

    // Collect per-field constraints up-front so we can short-circuit
    // when nothing on the struct opts into validation.
    let per_field: Vec<(&syn::Field, SchemaConstraints)> = fields_named
        .named
        .iter()
        .map(|f| (f, extract_schema_constraints(&f.attrs)))
        .collect();

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

    let field_blocks: Vec<TokenStream> = per_field
        .iter()
        .filter_map(|(field, constraints)| {
            let ident = field.ident.as_ref()?;
            emit_field_block(ident, &field.ty, constraints)
        })
        .collect();

    if field_blocks.is_empty() {
        return TokenStream::new();
    }

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
    field_ty: &Type,
    c: &SchemaConstraints,
) -> Option<TokenStream> {
    if !c.has_runtime_rule() {
        return None;
    }

    let field_name_str = field_ident.to_string();
    let numeric_kind = rust_numeric_kind(
        crate::schema_macro::type_utils::option_inner(field_ty).unwrap_or(field_ty),
    );
    let rule_blocks = emit_rule_blocks(c, &field_name_str, numeric_kind.as_deref());
    let dive_block = emit_dive_block(c);
    if rule_blocks.is_empty() && dive_block.is_empty() {
        return None;
    }

    let block = if crate::schema_macro::type_utils::is_option_type(field_ty) {
        // `field_ident` is `&Option<T>` after the `let Self { .. } = self` destructure.
        // Match ergonomics make `inner` end up as `&T`.
        quote! {
            {
                let mut __garde_path = ::vespera::__validation::garde::util::nested_path!(
                    __garde_path, #field_name_str
                );
                if let ::std::option::Option::Some(__garde_binding) = #field_ident {
                    #rule_blocks
                    #dive_block
                }
            }
        }
    } else {
        quote! {
            {
                let mut __garde_path = ::vespera::__validation::garde::util::nested_path!(
                    __garde_path, #field_name_str
                );
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
    field_name: &str,
    numeric_kind: Option<&str>,
) -> TokenStream {
    let mut blocks: Vec<TokenStream> = Vec::new();

    // ── String length (min_length / max_length → length::chars) ───────
    if c.min_length.is_some() || c.max_length.is_some() {
        let min = c.min_length.unwrap_or(0);
        let max = c.max_length.unwrap_or(usize::MAX);
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
        let max = c.max_items.unwrap_or(usize::MAX);
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
        let min_expr = numeric_some(c.minimum, numeric_kind);
        let max_expr = numeric_some(c.maximum, numeric_kind);
        blocks.push(quote! {
            if let ::std::result::Result::Err(__garde_error) =
                (::vespera::__validation::garde::rules::range::apply)(
                    __garde_binding,
                    (#min_expr, #max_expr),
                )
            {
                __garde_report.append(__garde_path(), __garde_error);
            }
        });
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

#[cfg(feature = "validation")]
fn numeric_some(value: Option<f64>, numeric_kind: Option<&str>) -> TokenStream {
    let Some(v) = value else {
        return quote! { ::std::option::Option::None };
    };

    // Render the literal in a form that matches the field type so the
    // garde `range::apply<T>` typeck succeeds.
    numeric_kind.map_or_else(
        // Unknown numeric kind — last-resort `as _` and let the user
        // see a compiler error pointing at their field type.
        || quote! { ::std::option::Option::Some(#v as _) },
        |kind| {
            let ty_ident = syn::Ident::new(kind, Span::call_site());
            let is_float = matches!(kind, "f32" | "f64");
            if !is_float && v.fract() == 0.0 && v.is_finite() {
                // Convert via i64 first so negative literals survive the
                // round-trip; the trailing `as #ty_ident` puts it into the
                // exact integer type garde's range::apply needs.
                #[allow(clippy::cast_possible_truncation)]
                let i = v as i64;
                quote! { ::std::option::Option::Some(#i as #ty_ident) }
            } else {
                quote! { ::std::option::Option::Some(#v as #ty_ident) }
            }
        },
    )
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
