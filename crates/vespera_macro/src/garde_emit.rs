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
    let numeric_kind = rust_numeric_kind(peel_option(field_ty).unwrap_or(field_ty));
    let rule_blocks = emit_rule_blocks(c, &field_name_str, numeric_kind.as_deref());
    let dive_block = emit_dive_block(c);
    if rule_blocks.is_empty() && dive_block.is_empty() {
        return None;
    }

    let block = if is_option_type(field_ty) {
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
        let static_ident = format_ident!("__VESPERA_PATTERN_{}", field_name.to_ascii_uppercase());
        blocks.push(quote! {
            {
                static #static_ident: ::std::sync::LazyLock<
                    ::vespera::__validation::garde::rules::pattern::regex::Regex,
                > = ::std::sync::LazyLock::new(|| {
                    ::vespera::__validation::garde::rules::pattern::regex::Regex::new(#pattern)
                        .expect("regex literal validated at vespera::Schema derive time")
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
fn is_option_type(ty: &Type) -> bool {
    crate::schema_macro::type_utils::option_inner(ty).is_some()
}

#[cfg(feature = "validation")]
fn peel_option(ty: &Type) -> Option<&Type> {
    crate::schema_macro::type_utils::option_inner(ty)
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
mod tests {
    use super::*;
    use syn::parse_quote;

    #[allow(clippy::needless_pass_by_value)] // test helper takes owned input by convention
    fn emit_to_string(input: DeriveInput) -> String {
        emit_garde_validate(&input).to_string()
    }

    #[test]
    fn no_constraints_emits_nothing() {
        let s: DeriveInput = parse_quote! {
            struct User {
                pub name: String,
                pub age: i32,
            }
        };
        assert!(emit_to_string(s).is_empty());
    }

    #[test]
    fn min_length_only_emits_length_chars_apply() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(min_length = 3)]
                pub name: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for User"));
        assert!(out.contains("length :: chars :: apply"));
        assert!(out.contains("3usize") || out.contains("3 usize"));
    }

    #[test]
    fn min_and_max_length_combined_in_single_call() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(min_length = 3, max_length = 32)]
                pub name: String,
            }
        };
        let out = emit_to_string(s);
        // single length::chars::apply call carrying both bounds
        let occurrences = out.matches("length :: chars :: apply").count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn range_emit_uses_field_numeric_type() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(minimum = 0, maximum = 150)]
                pub age: u32,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("range :: apply"));
        assert!(out.contains("as u32"));
    }

    #[test]
    fn range_emit_on_float_field_keeps_decimal_point() {
        let s: DeriveInput = parse_quote! {
            struct Price {
                #[schema(minimum = 0.01, maximum = 99.99)]
                pub amount: f64,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("range :: apply"));
        assert!(out.contains("as f64"));
    }

    #[test]
    fn pattern_emits_static_lazy_lock_regex() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(pattern = "^[a-z]+$")]
                pub username: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("static __VESPERA_PATTERN_USERNAME"));
        assert!(out.contains("LazyLock"));
        assert!(out.contains("regex :: Regex :: new"));
        assert!(out.contains("pattern :: apply"));
    }

    #[test]
    fn format_email_emits_email_apply() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(format = "email")]
                pub email: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("email :: apply"));
    }

    #[test]
    fn format_uri_emits_url_apply() {
        let s: DeriveInput = parse_quote! {
            struct Site {
                #[schema(format = "uri")]
                pub home: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("url :: apply"));
    }

    #[test]
    fn format_ipv4_emits_ip_apply_with_v4_kind() {
        let s: DeriveInput = parse_quote! {
            struct Host {
                #[schema(format = "ipv4")]
                pub addr: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("ip :: apply"));
        assert!(out.contains("IpKind :: V4"));
    }

    #[test]
    fn format_uuid_is_annotation_only_no_runtime_rule() {
        let s: DeriveInput = parse_quote! {
            struct Entity {
                #[schema(format = "uuid")]
                pub id: String,
            }
        };
        // uuid alone has no garde rule → no Validate impl emitted.
        assert!(emit_to_string(s).is_empty());
    }

    #[test]
    fn option_field_wraps_rule_block_in_if_let_some() {
        let s: DeriveInput = parse_quote! {
            struct User {
                #[schema(min_length = 3)]
                pub nickname: Option<String>,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("if let :: std :: option :: Option :: Some"));
        assert!(out.contains("length :: chars :: apply"));
    }

    #[test]
    fn min_max_items_on_vec_emits_length_simple() {
        let s: DeriveInput = parse_quote! {
            struct Post {
                #[schema(min_items = 1, max_items = 5)]
                pub tags: Vec<String>,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("length :: simple :: apply"));
    }

    #[test]
    fn enum_emits_nothing() {
        let e: DeriveInput = parse_quote! {
            enum Status { Active, Inactive }
        };
        assert!(emit_to_string(e).is_empty());
    }

    #[test]
    fn tuple_struct_emits_nothing() {
        let s: DeriveInput = parse_quote! {
            struct Wrapper(pub String);
        };
        assert!(emit_to_string(s).is_empty());
    }

    #[test]
    fn unit_struct_emits_nothing() {
        let s: DeriveInput = parse_quote! {
            struct Empty;
        };
        assert!(emit_to_string(s).is_empty());
    }

    #[test]
    fn generic_struct_with_constraints_produces_compile_error() {
        let s: DeriveInput = parse_quote! {
            struct Wrapper<T> {
                #[schema(min_length = 3)]
                pub name: String,
                pub inner: T,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("compile_error"));
        assert!(out.contains("generic"));
    }

    #[test]
    fn annotation_only_constraints_emit_nothing() {
        // example / read_only / write_only / unique_items / multiple_of /
        // exclusive bounds are OpenAPI annotations only; they should not
        // drag a Validate impl into existence on their own.
        let s: DeriveInput = parse_quote! {
            struct Doc {
                #[schema(read_only, example = "abc", unique_items, multiple_of = 0.5)]
                pub id: String,
            }
        };
        assert!(emit_to_string(s).is_empty());
    }

    // ── nested validation (`#[schema(dive)]`) emission ──────────────

    #[test]
    fn dive_on_plain_field_emits_validate_into_call() {
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(dive)]
                pub address: Address,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for Order"));
        assert!(out.contains("Validate :: validate_into"));
        assert!(out.contains("\"address\""));
    }

    #[test]
    fn dive_on_option_wraps_in_if_let_some() {
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(dive)]
                pub address: Option<Address>,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("if let :: std :: option :: Option :: Some"));
        assert!(out.contains("Validate :: validate_into"));
    }

    #[test]
    fn dive_on_vec_emits_single_validate_into_call() {
        // garde's runtime `Vec<T>: Validate` impl iterates and pushes
        // `[idx]` path components automatically — the macro only emits
        // one `validate_into` call regardless of container kind.
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(dive)]
                pub items: Vec<LineItem>,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("Validate :: validate_into"));
        // `validate_into` appears twice: once as the outer fn declaration
        // (`fn validate_into(...)`) and once as the inner trait dispatch
        // (`Validate :: validate_into(...)`).  Anything more would mean
        // the macro is iterating itself, which is what we explicitly
        // delegate to garde's runtime `Vec<T>: Validate` impl.
        assert_eq!(
            out.matches("validate_into").count(),
            2,
            "expected outer fn + one inner trait call; iteration is garde-runtime, \
             so the macro must NOT emit a `for` loop"
        );
        // `for` keyword appears in `impl ... for Order` — count only
        // tokens that look like loop iteration (`for <ident> in `).
        let loop_count = out.matches("in __garde_binding").count();
        assert_eq!(loop_count, 0, "macro must not emit explicit iteration");
    }

    #[test]
    fn dive_combined_with_length_emits_both_rules() {
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(min_items = 1, max_items = 10, dive)]
                pub items: Vec<LineItem>,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("length :: simple :: apply"));
        assert!(out.contains("Validate :: validate_into"));
    }

    #[test]
    fn dive_false_disables_emission() {
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(dive = false)]
                pub address: Address,
            }
        };
        // `dive = false` is the same as no annotation — no rule
        // produced means no `impl Validate` emitted.
        assert!(emit_to_string(s).is_empty());
    }

    // ── format=ipv6 / format=ip / unknown format ────────────────────

    #[test]
    fn format_ipv6_emits_ip_apply_with_v6_kind() {
        let s: DeriveInput = parse_quote! {
            struct Host {
                #[schema(format = "ipv6")]
                pub addr: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("ip :: apply"));
        assert!(out.contains("IpKind :: V6"));
    }

    #[test]
    fn format_ip_emits_ip_apply_with_any_kind() {
        let s: DeriveInput = parse_quote! {
            struct Host {
                #[schema(format = "ip")]
                pub addr: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("ip :: apply"));
        assert!(out.contains("IpKind :: Any"));
    }

    #[test]
    fn format_url_alias_emits_url_apply() {
        // `format = "url"` is the documented alias for `"uri"` —
        // both must dispatch to garde's `url::apply`.
        let s: DeriveInput = parse_quote! {
            struct Site {
                #[schema(format = "url")]
                pub home: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("url :: apply"));
    }

    #[test]
    fn unknown_format_with_other_rule_skips_format_branch() {
        // Combining an unsupported `format = "custom"` with a known
        // runtime rule (`min_length = 3`) forces the emitter to enter
        // `emit_rule_blocks` AND fall through the unknown-format
        // branch — exercising the `_ => {}` arm.
        let s: DeriveInput = parse_quote! {
            struct Doc {
                #[schema(min_length = 3, format = "custom-thing")]
                pub id: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("length :: chars :: apply"));
        // The unknown format MUST NOT produce any `ip::`/`email::`/
        // `url::` call — confirms the `_ => {}` arm took effect.
        assert!(!out.contains("ip :: apply"));
        assert!(!out.contains("email :: apply"));
        assert!(!out.contains("url :: apply"));
    }

    // ── mixed-field structs exercising the no-runtime-rule early exit
    //    inside emit_field_block ────────────────────────────────────

    #[test]
    fn mixed_validated_and_unvalidated_fields_emit_only_validated_blocks() {
        // `a` has a runtime rule; `b` does not.  emit_field_block must
        // hit its early `return None` for `b` while still emitting `a`.
        let s: DeriveInput = parse_quote! {
            struct Mixed {
                #[schema(min_length = 3)]
                pub a: String,
                pub b: String,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for Mixed"));
        assert!(out.contains("\"a\""));
        // Field `b` has no constraint — no path literal should appear.
        assert!(!out.contains("\"b\""));
    }

    // ── one-sided numeric bounds exercising numeric_some(None, _) ───

    #[test]
    fn only_minimum_set_emits_none_for_max_bound() {
        let s: DeriveInput = parse_quote! {
            struct N {
                #[schema(minimum = 0)]
                pub n: u32,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("range :: apply"));
        // The missing upper bound must serialize as Option::None.
        assert!(out.contains("Option :: None"));
    }

    #[test]
    fn only_maximum_set_emits_none_for_min_bound() {
        let s: DeriveInput = parse_quote! {
            struct N {
                #[schema(maximum = 100)]
                pub n: u32,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("range :: apply"));
        assert!(out.contains("Option :: None"));
    }

    // ── numeric_some with unknown numeric_kind (non-primitive field) ─

    #[test]
    fn minimum_on_non_primitive_field_falls_back_to_as_wildcard() {
        // Field type is a user-defined `Money` newtype — peel_option
        // returns None and rust_numeric_kind returns None, forcing
        // numeric_some down the `as _` fallback branch.
        let s: DeriveInput = parse_quote! {
            struct Order {
                #[schema(minimum = 0)]
                pub price: Money,
            }
        };
        let out = emit_to_string(s);
        assert!(out.contains("range :: apply"));
        assert!(
            out.contains("as _"),
            "non-primitive field should emit `as _` fallback, got: {out}"
        );
    }

    // ── is_option_type / peel_option / rust_numeric_kind branches ───

    #[test]
    fn tuple_typed_field_does_not_trip_option_or_numeric_helpers() {
        // Tuple types are Type::Tuple, not Type::Path — drives the
        // non-Path early-return branches inside is_option_type,
        // peel_option, and rust_numeric_kind.
        let s: DeriveInput = parse_quote! {
            struct WithTuple {
                #[schema(min_length = 3)]
                pub x: (String,),
            }
        };
        let out = emit_to_string(s);
        // Tuple is not an Option — outer rule block must NOT wrap in
        // `if let Some`.
        assert!(!out.contains("if let :: std :: option :: Option :: Some"));
        assert!(out.contains("length :: chars :: apply"));
    }

    #[test]
    fn bare_option_without_angle_brackets_falls_through_peel() {
        // A bare `Option` with no type argument (invalid Rust, but the
        // macro must still handle it gracefully without panicking).
        // Detection now goes through `option_inner`, which extracts the
        // inner type from `Option<T>`; a bare `Option` has no inner type,
        // so `is_option_type` returns false and the field is NOT treated
        // as a peelable option.  The rule is therefore applied directly
        // (`else` branch) rather than wrapped in `if let Some`.
        let s: DeriveInput = parse_quote! {
            struct BareOption {
                #[schema(min_length = 3)]
                pub x: Option,
            }
        };
        let out = emit_to_string(s);
        // No panic; not peeled, so no `if let Some` wrap …
        assert!(!out.contains("if let :: std :: option :: Option :: Some"));
        // … but the length rule is still emitted (applied directly).
        assert!(out.contains("length :: chars :: apply"));
    }

    #[test]
    fn option_with_lifetime_only_arg_falls_through_find_map() {
        // `Option<'static>` is syntactically a valid path with one
        // angle-bracketed argument — but the argument is a Lifetime,
        // not a Type, so peel_option's `find_map` returns None.
        // Semantically nonsensical, but the macro must not panic.
        let s: DeriveInput = parse_quote! {
            struct WithLifetime {
                #[schema(min_length = 3)]
                pub x: Option<'static>,
            }
        };
        let out = emit_to_string(s);
        // The rule block still emits — peel_option returning None just
        // means rust_numeric_kind is invoked on the outer `Option<'a>`
        // type, which also returns None.  No panic, no compile_error.
        assert!(out.contains("length :: chars :: apply"));
    }
}
