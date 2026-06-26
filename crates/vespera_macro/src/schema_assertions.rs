//! Compile-time supplements emitted by `#[derive(Schema)]`.
//!
//! Every `#[derive(Schema)]` invocation produces two extra token
//! streams in addition to the metadata registration:
//!
//! 1. **A marker-trait impl** — `impl ::vespera::Schema for #Ident {}`
//!    (preserving the struct's generics).  The `Schema` trait is an
//!    intentionally empty marker defined in `vespera::lib.rs`; it
//!    coexists with the `vespera::Schema` derive macro because traits
//!    and macros live in separate namespaces.  This anchor lets the
//!    leaf-type assertions below resolve their `T: ::vespera::Schema`
//!    bound for every type that opted in via `#[derive(Schema)]`,
//!    whether the type lives in the user crate or in an external crate
//!    that the OpenAPI generator does not know about.
//!
//! 2. **Per-field leaf-type assertions** — for each field whose leaf
//!    type is NOT a recognized OpenAPI builtin (per
//!    [`is_builtin_openapi_type`]), NOT `serde_json::Value`, NOT a
//!    generic type parameter of the current struct, and NOT marked
//!    with `#[schema(any)]`, this module emits a const block:
//!
//!    ```ignore
//!    const _: () = {
//!        const fn __vespera_assert_schema<T: ::vespera::Schema>() {}
//!        let _ = __vespera_assert_schema::<#leaf_ty>;
//!    };
//!    ```
//!
//!    The assertion is `quote_spanned!`-ed at the field's span so the
//!    compile error points at the offending field — a forgotten
//!    `#[derive(Schema)]` on a custom type becomes a loud build break
//!    instead of a silent `{type:"object"}` in `openapi.json`.
//!
//! Both supplements are additive: they do not change the generated
//! OpenAPI byte for any non-opaque field.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{GenericArgument, PathArguments, Type};

use crate::parser::schema::{is_builtin_openapi_type, schema_attrs::SchemaConstraints};

/// Emit BOTH the marker-trait impl AND the per-field leaf-type
/// assertions for `input` — the single entry point called from
/// `process_derive_schema`.  Returns an empty `TokenStream` only when
/// the input shape would otherwise miscompile (e.g. parse-failed
/// `#[schema(...)]` attrs on a field — surfaced upstream as a
/// `compile_error!`).
///
/// `field_constraints` is the slice of already-parsed `#[schema(...)]`
/// values produced by `process_derive_schema` in a single walk; it is
/// indexed pairwise with `fields_named.named` inside
/// `emit_field_assertions`. Passing the slice avoids re-running
/// `try_extract_schema_constraints` here (the parser had already been
/// invoked once per field for validation).
#[must_use]
pub fn emit_schema_supplements(
    input: &syn::DeriveInput,
    field_constraints: &[SchemaConstraints],
) -> TokenStream {
    let marker = emit_marker_impl(input);
    let assertions = emit_field_assertions(input, field_constraints);
    quote! {
        #marker
        #assertions
    }
}

/// Emit `impl ::vespera::Schema for #Ident {}`, preserving every
/// generic parameter, lifetime, and where clause on the input.  The
/// marker trait is empty, so the impl is unconditional — no synthetic
/// `where T: Schema` bound is added (that would over-constrain
/// existing generic structs).
fn emit_marker_impl(input: &syn::DeriveInput) -> TokenStream {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        #[automatically_derived]
        impl #impl_generics ::vespera::Schema for #name #ty_generics #where_clause {}
    }
}

/// Walk the struct's named fields and emit a `const _: () = { ... };`
/// block per non-exempt leaf type.  Exemptions:
///
/// - Field is marked `#[schema(any)]` (entire field is skipped).
/// - Leaf path's last segment is a builtin OpenAPI ident
///   (per [`is_builtin_openapi_type`]).
/// - Leaf is `serde_json::Value` (bare `Value` OR any path containing
///   a `serde_json` segment ending in `Value`).
/// - Leaf type's path references any generic parameter of the parent
///   struct — those can only be checked at the use site where the
///   generic is instantiated (the parent's marker impl is unconditional
///   so an unbound generic does not break the trait bound here).
///
/// Returns an empty stream for non-structs, non-named-field structs,
/// and structs where every leaf is exempt — keeping the existing
/// emitted bytes for those cases byte-identical.
///
/// `field_constraints` is the pre-parsed `#[schema(...)]` slice (one
/// entry per named field, in declaration order). `process_derive_schema`
/// already collected it during the per-field validation walk, so
/// indexing pairwise with `fields_named.named` here removes a
/// duplicate parse pass over every field's attrs.
fn emit_field_assertions(
    input: &syn::DeriveInput,
    field_constraints: &[SchemaConstraints],
) -> TokenStream {
    let syn::Data::Struct(data_struct) = &input.data else {
        return TokenStream::new();
    };
    let syn::Fields::Named(fields_named) = &data_struct.fields else {
        return TokenStream::new();
    };

    let generic_idents: HashSet<String> = input
        .generics
        .params
        .iter()
        .filter_map(|param| {
            if let syn::GenericParam::Type(t) = param {
                Some(t.ident.to_string())
            } else {
                None
            }
        })
        .collect();

    let mut blocks: Vec<TokenStream> = Vec::new();
    // `process_derive_schema` constructs `field_constraints` with exactly one
    // entry per named field, in declaration order. `zip` therefore yields the
    // full pairing here; when the caller forwards an empty slice (e.g. unit /
    // tuple structs short-circuit above never reach this loop) we degenerate
    // to zero iterations like the previous code.
    for (field, constraints) in fields_named.named.iter().zip(field_constraints) {
        // `#[schema(any)]` is the documented escape hatch — the field
        // explicitly opted out of type-driven schema generation, so
        // there's no leaf to assert.  Parse errors on `#[schema(...)]`
        // are surfaced upstream by `process_derive_schema`, so a field
        // that would have produced an `Err` here never makes it into
        // `field_constraints` (the early `to_compile_error` return
        // upstream supersedes the `continue` the previous code took).
        if constraints.is_any() {
            continue;
        }

        for leaf in collect_leaf_custom_types(&field.ty) {
            if type_contains_generic(&leaf, &generic_idents) {
                continue;
            }
            let Some(last_ident) = last_path_ident(&leaf) else {
                continue;
            };
            if is_builtin_openapi_type(&last_ident) {
                continue;
            }
            if is_serde_json_value_leaf(&leaf) {
                continue;
            }
            // Span the entire `const _ : () = { ... };` block to the
            // field's span (the design pins the diagnostic to the
            // field, not just its type, so `field.span()` covers the
            // whole `name: Type` declaration).  The const item name is
            // `_` so multiple assertions per struct never collide.
            let field_span = field.span();
            blocks.push(quote_spanned! { field_span =>
                const _: () = {
                    const fn __vespera_assert_schema<T: ::vespera::Schema>() {}
                    let _ = __vespera_assert_schema::<#leaf>;
                };
            });
        }
    }

    quote! { #(#blocks)* }
}

/// Recursively unwrap the "transparent for schema" containers
/// (`Vec<_>`, `Option<_>`, `Box<_>`, `HashSet<_>`, `BTreeSet<_>`,
/// `HashMap<_, V>`, `BTreeMap<_, V>`) and return the LEAF type that
/// the assertion should fire on.  Maps return the VALUE type (the key
/// is the JSON object key, a string by convention).
///
/// Returns an empty `Vec` for type shapes the macro cannot assert on
/// (references, slices, tuples, function pointers, …): those already
/// fall through to `Object` in the generator and there's no scalar
/// "leaf" identifier to bind a bound to.
fn collect_leaf_custom_types(ty: &Type) -> Vec<Type> {
    let Type::Path(type_path) = ty else {
        return Vec::new();
    };
    let Some(segment) = type_path.path.segments.last() else {
        return Vec::new();
    };

    let ident = segment.ident.to_string();
    match ident.as_str() {
        "Vec" | "Option" | "Box" | "HashSet" | "BTreeSet" => {
            return first_generic_type_arg(segment)
                .map(collect_leaf_custom_types)
                .unwrap_or_default();
        }
        "HashMap" | "BTreeMap" => {
            return second_generic_type_arg(segment)
                .map(collect_leaf_custom_types)
                .unwrap_or_default();
        }
        _ => {}
    }
    vec![ty.clone()]
}

fn first_generic_type_arg(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| {
        if let GenericArgument::Type(inner) = arg {
            Some(inner)
        } else {
            None
        }
    })
}

fn second_generic_type_arg(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args
        .iter()
        .filter_map(|arg| {
            if let GenericArgument::Type(inner) = arg {
                Some(inner)
            } else {
                None
            }
        })
        .nth(1)
}

fn last_path_ident(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    type_path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

/// `true` when any path segment of `ty` (including inside nested
/// `<...>` arguments) names one of the current struct's generic type
/// parameters.  We cannot synthesise a trait bound for a generic
/// parameter from inside the derive — the marker impl is unconditional
/// `impl<T> Schema for Foo<T> {}` — so any leaf referring to `T` must
/// be skipped.
fn type_contains_generic(ty: &Type, generic_idents: &HashSet<String>) -> bool {
    match ty {
        Type::Path(type_path) => {
            for segment in &type_path.path.segments {
                if generic_idents.contains(&segment.ident.to_string()) {
                    return true;
                }
                if let PathArguments::AngleBracketed(args) = &segment.arguments {
                    for arg in &args.args {
                        if let GenericArgument::Type(inner) = arg
                            && type_contains_generic(inner, generic_idents)
                        {
                            return true;
                        }
                    }
                }
            }
            false
        }
        Type::Reference(r) => type_contains_generic(&r.elem, generic_idents),
        _ => false,
    }
}

/// `serde_json::Value` allowlist — bare `Value`, or any qualified path
/// whose final segment is `Value` and which contains a `serde_json`
/// segment somewhere (catches `serde_json::Value`, `::serde_json::Value`,
/// and `vespera::serde_json::Value` — vespera re-exports serde_json).
fn is_serde_json_value_leaf(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let segments = &type_path.path.segments;
    // Reject early on the trivial last-segment check so non-`Value`
    // leaves (the overwhelming common case) pay zero allocation.
    // `syn::Ident: PartialEq<str>` is a direct byte compare — no
    // intermediate `String`.
    let Some(last) = segments.last() else {
        return false;
    };
    if last.ident != "Value" {
        return false;
    }
    // Bare `Value` — allowlisted (matches design literal "match the
    // last segment `Value`").
    if segments.len() == 1 {
        return true;
    }
    // Multi-segment path: require a `serde_json` segment anywhere so
    // we don't allowlist an unrelated custom `foo::Bar::Value`.  This
    // catches `serde_json::Value` (2-segment, the canonical case),
    // `::serde_json::Value` (3-segment with leading colon stripped),
    // and `vespera::serde_json::Value` (3-segment re-export path).
    segments.iter().any(|segment| segment.ident == "serde_json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;
    use syn::parse_quote;

    fn parse_ty(src: &str) -> Type {
        syn::parse_str::<Type>(src).expect("valid type")
    }

    fn leaves_of(src: &str) -> Vec<String> {
        collect_leaf_custom_types(&parse_ty(src))
            .iter()
            .map(|ty| quote!(#ty).to_string().replace(' ', ""))
            .collect()
    }

    /// Recompute the per-field `SchemaConstraints` slice the way
    /// `process_derive_schema` does in production code, so tests can
    /// drive `emit_field_assertions` (which now takes the slice as a
    /// parameter) without re-stating the parse pass at every call site.
    fn constraints_for(input: &syn::DeriveInput) -> Vec<SchemaConstraints> {
        use crate::parser::schema::schema_attrs::try_extract_schema_constraints;
        if let syn::Data::Struct(d) = &input.data
            && let syn::Fields::Named(f) = &d.fields
        {
            return f
                .named
                .iter()
                .map(|fld| try_extract_schema_constraints(&fld.attrs).unwrap_or_default())
                .collect();
        }
        Vec::new()
    }

    // ── collect_leaf_custom_types ──────────────────────────────────

    #[test]
    fn leaf_of_plain_path_is_itself() {
        assert_eq!(leaves_of("MyType"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_vec_unwraps_inner() {
        assert_eq!(leaves_of("Vec<MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_option_unwraps_inner() {
        assert_eq!(leaves_of("Option<MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_box_unwraps_inner() {
        assert_eq!(leaves_of("Box<MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_hashset_unwraps_inner() {
        assert_eq!(leaves_of("HashSet<MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_btreeset_unwraps_inner() {
        assert_eq!(leaves_of("BTreeSet<MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_hashmap_unwraps_value_not_key() {
        // Key is always String by JSON convention; only the value gets
        // an OpenAPI schema reference, so only the value gets asserted.
        assert_eq!(leaves_of("HashMap<String, MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_btreemap_unwraps_value_not_key() {
        assert_eq!(leaves_of("BTreeMap<String, MyType>"), vec!["MyType"]);
    }

    #[test]
    fn leaf_of_nested_wrappers() {
        assert_eq!(
            leaves_of("Vec<Option<Box<HashMap<String, MyType>>>>"),
            vec!["MyType"]
        );
    }

    #[test]
    fn leaf_of_non_path_type_is_empty() {
        // References, slices, tuples produce no leaf to assert on.
        assert!(collect_leaf_custom_types(&parse_ty("(i32, String)")).is_empty());
        assert!(collect_leaf_custom_types(&parse_ty("&MyType")).is_empty());
    }

    #[test]
    fn leaf_of_empty_wrapper_is_empty() {
        // `Vec` with no type args (degenerate) has no leaf.
        assert!(collect_leaf_custom_types(&parse_ty("Vec")).is_empty());
    }

    // ── is_serde_json_value_leaf ────────────────────────────────────

    #[test]
    fn bare_value_is_allowlisted() {
        assert!(is_serde_json_value_leaf(&parse_ty("Value")));
    }

    #[test]
    fn serde_json_value_is_allowlisted() {
        assert!(is_serde_json_value_leaf(&parse_ty("serde_json::Value")));
    }

    #[test]
    fn leading_colon_serde_json_value_is_allowlisted() {
        assert!(is_serde_json_value_leaf(&parse_ty("::serde_json::Value")));
    }

    #[test]
    fn vespera_reexport_serde_json_value_is_allowlisted() {
        assert!(is_serde_json_value_leaf(&parse_ty(
            "vespera::serde_json::Value"
        )));
    }

    #[test]
    fn unrelated_value_segment_is_not_allowlisted() {
        // `foo::bar::Value` is some user enum named Value — not the
        // serde_json sentinel, so the strict-schema check still fires.
        assert!(!is_serde_json_value_leaf(&parse_ty("foo::bar::Value")));
    }

    #[test]
    fn last_segment_not_value_is_not_allowlisted() {
        assert!(!is_serde_json_value_leaf(&parse_ty("serde_json::Map")));
    }

    // ── type_contains_generic ───────────────────────────────────────

    #[test]
    fn generic_param_directly() {
        let mut generics = HashSet::new();
        generics.insert("T".to_string());
        assert!(type_contains_generic(&parse_ty("T"), &generics));
    }

    #[test]
    fn generic_param_inside_vec() {
        let mut generics = HashSet::new();
        generics.insert("T".to_string());
        assert!(type_contains_generic(&parse_ty("Vec<T>"), &generics));
    }

    #[test]
    fn generic_param_inside_box_inside_option() {
        let mut generics = HashSet::new();
        generics.insert("T".to_string());
        assert!(type_contains_generic(
            &parse_ty("Option<Box<HashMap<String, T>>>"),
            &generics
        ));
    }

    #[test]
    fn no_generic_param_present() {
        let mut generics = HashSet::new();
        generics.insert("T".to_string());
        assert!(!type_contains_generic(&parse_ty("Vec<String>"), &generics));
        assert!(!type_contains_generic(
            &parse_ty("HashMap<String, MyType>"),
            &generics
        ));
    }

    #[test]
    fn empty_generics_set_never_matches() {
        let generics = HashSet::new();
        assert!(!type_contains_generic(&parse_ty("T"), &generics));
    }

    // ── emit_marker_impl preserves generics ─────────────────────────

    #[test]
    fn marker_impl_for_plain_struct() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Plain { x: i32 }
        };
        let out = emit_marker_impl(&input).to_string();
        assert!(out.contains("impl :: vespera :: Schema for Plain"));
    }

    #[test]
    fn marker_impl_preserves_generics_and_bounds() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Container<T: Serialize> { value: T }
        };
        let out = emit_marker_impl(&input).to_string();
        assert!(out.contains("impl < T : Serialize > :: vespera :: Schema for Container < T >"));
    }

    #[test]
    fn marker_impl_for_enum() {
        let input: syn::DeriveInput = parse_quote! {
            pub enum E { A, B(i32) }
        };
        let out = emit_marker_impl(&input).to_string();
        assert!(out.contains("impl :: vespera :: Schema for E"));
    }

    // ── emit_field_assertions exemption surface ─────────────────────

    #[test]
    fn no_assertion_for_builtin_only_struct() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Plain {
                id: i32,
                name: String,
                tags: Vec<String>,
                map: HashMap<String, bool>,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(
            out.is_empty(),
            "builtins-only struct must emit zero assertions, got: {out}"
        );
    }

    #[test]
    fn assertion_for_unknown_custom_leaf() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Holder {
                inner: MyUnknown,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(
            out.contains("__vespera_assert_schema") && out.contains("MyUnknown"),
            "expected assertion on MyUnknown, got: {out}"
        );
    }

    #[test]
    fn no_assertion_for_serde_json_value() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Holder {
                raw: serde_json::Value,
                bare: Value,
                via: Option<Vec<serde_json::Value>>,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(
            out.is_empty(),
            "Value fields must not emit assertions, got: {out}"
        );
    }

    #[test]
    fn no_assertion_for_any_marked_field() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Holder {
                #[schema(any)]
                opaque: MyUnknown,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(
            out.is_empty(),
            "`#[schema(any)]` must skip the assertion, got: {out}"
        );
    }

    #[test]
    fn no_assertion_for_generic_parameter_leaf() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Generic<T> {
                value: T,
                vec: Vec<T>,
                map: HashMap<String, T>,
                name: String,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(
            out.is_empty(),
            "fields whose leaf is the struct's generic param must not emit assertions, got: {out}"
        );
    }

    #[test]
    fn assertion_per_unknown_field_individually() {
        let input: syn::DeriveInput = parse_quote! {
            pub struct Holder {
                a: AlphaType,
                b: Vec<BetaType>,
                c: HashMap<String, GammaType>,
                d: i32,
                #[schema(any)]
                e: WhateverType,
            }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        // Three assertions (a, b, c) for the three non-exempt leaves.
        assert_eq!(
            out.matches("let _ = __vespera_assert_schema").count(),
            3,
            "expected one assertion per non-exempt field: {out}"
        );
        assert!(out.contains("AlphaType"));
        assert!(out.contains("BetaType"));
        assert!(out.contains("GammaType"));
        assert!(!out.contains("WhateverType"));
    }

    #[test]
    fn no_assertion_for_non_struct_kinds() {
        // Enums get the marker impl (separately) but the per-variant
        // field walk is intentionally out of scope for v1; their
        // variant fields fall back to silent `Object` as before.
        let input: syn::DeriveInput = parse_quote! {
            pub enum E { A(MyUnknown) }
        };
        assert!(
            emit_field_assertions(&input, &constraints_for(&input))
                .to_string()
                .is_empty()
        );

        let input: syn::DeriveInput = parse_quote! {
            pub struct Tuple(MyUnknown);
        };
        assert!(
            emit_field_assertions(&input, &constraints_for(&input))
                .to_string()
                .is_empty()
        );

        let input: syn::DeriveInput = parse_quote! {
            pub struct Unit;
        };
        assert!(
            emit_field_assertions(&input, &constraints_for(&input))
                .to_string()
                .is_empty()
        );
    }
}
