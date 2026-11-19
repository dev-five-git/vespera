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
//!    generic type parameter of the current struct, NOT `PhantomData` (std's;
//!    see the exemptions of `emit_field_assertions`), and NOT marked
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
use syn::visit::Visit;
use syn::{GenericArgument, PathArguments, Type};

use crate::parser::schema::{
    SerdeSide, extract_field_codec, extract_skip, is_builtin_openapi_type,
    schema_attrs::SchemaConstraints, wire_override::container_override_type,
};
use crate::schema_macro::is_extern_crate;

mod map_leaf;
use map_leaf::is_serde_json_map_leaf;

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

/// Emit a `const _: () = { ... };` block per non-exempt leaf type of every
/// type the schema documents.  Exemptions:
///
/// - Field is marked `#[schema(any)]` (entire field is skipped).
/// - Field has `#[schema(value_type = T)]`: `T`'s leaves are asserted
///   instead of the field type's.
/// - Field is converted by serde `with` (or both `serialize_with` and
///   `deserialize_with`) without a `value_type`: documented as any value, so
///   its Rust type is never consulted.
/// - Leaf path's last segment is a builtin OpenAPI ident
///   (per [`is_builtin_openapi_type`]).
/// - Leaf's last segment is `PhantomData` and the path is not a user-module
///   path: std's `PhantomData` has no `Schema` impl and serde writes it as a
///   unit.
/// - Leaf is `serde_json::Value` (bare `Value`, SeaORM's `JsonValue`, OR
///   any path containing a `serde_json` segment ending in `Value`).
/// - Leaf type's path references any generic parameter of the parent
///   struct — those can only be checked at the use site where the
///   generic is instantiated (the parent's marker impl is unconditional
///   so an unbound generic does not break the trait bound here).
///
/// Container-level `#[schema(value_type = T)]` / serde `into` / `from` /
/// `try_from` types are asserted too (structs and enums); when they cover
/// both directions the fields are never documented and are not asserted.
///
/// Returns an empty stream for non-named-field structs and enums without a
/// container override, and structs where every leaf is exempt — keeping the
/// existing emitted bytes for those cases byte-identical.
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
    // Every generic parameter kind: a type `T`, a const `N`, a lifetime
    // (`"'a"`) — none of them is in scope in a module-level assertion.
    let generic_idents: HashSet<String> = input
        .generics
        .params
        .iter()
        .map(|param| match param {
            syn::GenericParam::Type(t) => t.ident.to_string(),
            syn::GenericParam::Const(c) => c.ident.to_string(),
            syn::GenericParam::Lifetime(l) => format!("'{}", l.lifetime.ident),
        })
        .collect();

    let mut blocks: Vec<TokenStream> = Vec::new();
    let overrides = [SerdeSide::Serialize, SerdeSide::Deserialize]
        .map(|side| container_override_type(&input.attrs, side));
    for ty in overrides.iter().flatten() {
        push_leaf_assertions(ty, ty.span(), &generic_idents, &mut blocks);
    }
    let fields_documented = overrides.iter().any(Option::is_none);
    if fields_documented
        && let syn::Data::Struct(data_struct) = &input.data
        && let syn::Fields::Named(fields_named) = &data_struct.fields
    {
        // `process_derive_schema` constructs `field_constraints` with exactly
        // one entry per named field, in declaration order, so `zip` yields
        // the full pairing.
        for (field, constraints) in fields_named.named.iter().zip(field_constraints) {
            // `#[schema(any)]` is the documented escape hatch — the field
            // explicitly opted out of type-driven schema generation, so
            // there's no leaf to assert.  Parse errors on `#[schema(...)]`
            // are surfaced upstream by `process_derive_schema`.
            // A fully `#[serde(skip)]`-ped field never reaches the schema
            // (a `PgPool` handle, a cache), so its type is never consulted.
            if constraints.is_any() || extract_skip(&field.attrs) {
                continue;
            }
            let codec = extract_field_codec(&field.attrs);
            let documented = match &constraints.value_type {
                Some(value_type) => &value_type.0,
                None if codec.serialize && codec.deserialize => continue,
                None => &field.ty,
            };
            // Span the entire `const _ : () = { ... };` block to the
            // field's span (the design pins the diagnostic to the field,
            // not just its type, so `field.span()` covers the whole
            // `name: Type` declaration).
            push_leaf_assertions(documented, field.span(), &generic_idents, &mut blocks);
        }
    }

    quote! { #(#blocks)* }
}

/// Push one `T: ::vespera::Schema` assertion per non-exempt leaf of `ty`.
fn push_leaf_assertions(
    ty: &Type,
    span: proc_macro2::Span,
    generic_idents: &HashSet<String>,
    blocks: &mut Vec<TokenStream>,
) {
    for (leaf, last_ident) in collect_leaf_custom_types(ty) {
        // A builtin-named leaf is the builtin only when it is bare or an
        // external crate's path; a user module's `api::Header` is a type of
        // its own, so it is checked like any other custom leaf.
        let builtin = is_builtin_openapi_type(&last_ident) && !is_user_module_path(&leaf);
        // `PhantomData<T>` has no `Schema` impl (std type), and serde writes it
        // as a unit (`null`): it is exempt here, not in the conversion builtin
        // list (`is_builtin_openapi_type`), whose leaf arms must stay in sync.
        let phantom = last_ident == "PhantomData" && !is_user_module_path(&leaf);
        if type_contains_generic(&leaf, generic_idents)
            || builtin
            || phantom
            || is_serde_json_value_leaf(&leaf)
            || is_serde_json_map_leaf(&leaf)
        {
            continue;
        }
        // The const item name is `_` so multiple assertions per struct
        // never collide.
        blocks.push(quote_spanned! { span =>
            const _: () = {
                const fn __vespera_assert_schema<T: ::vespera::Schema>() {}
                let _ = __vespera_assert_schema::<#leaf>;
            };
        });
    }
}

/// Recursively unwrap the "transparent for schema" containers
/// (`Vec<_>`, `Option<_>`, `Box<_>`, `HashSet<_>`, `BTreeSet<_>`,
/// `HashMap<_, V>`, `BTreeMap<_, V>`) and return the LEAF type that
/// the assertion should fire on.  Maps return the VALUE type (the key
/// is the JSON object key, a string by convention).
///
/// References, slices, arrays, tuples, parentheses and `macro_rules!`
/// `$t:ty` groups are walked into like the generator does (a tuple is a
/// fixed-length array of its elements, `[T; N]` / `&[T]` an array of `T`,
/// `&T` is `T`), so an underived custom type there is still a compile error
/// instead of a silent `{}`.  Shapes with no JSON form of their own
/// (function pointers, `impl Trait`, `dyn Trait`, `!`) yield nothing.
/// Each leaf is paired with its final path ident.  The early returns below
/// reject the only shapes that lack one, so the caller never has to handle a
/// missing ident - the invariant lives here instead of as an unreachable
/// `else` branch at the call site.
fn collect_leaf_custom_types(ty: &Type) -> Vec<(Type, String)> {
    let type_path = match ty {
        Type::Path(type_path) => type_path,
        Type::Reference(inner) => return collect_leaf_custom_types(&inner.elem),
        Type::Slice(inner) => return collect_leaf_custom_types(&inner.elem),
        Type::Array(inner) => return collect_leaf_custom_types(&inner.elem),
        Type::Paren(inner) => return collect_leaf_custom_types(&inner.elem),
        Type::Group(inner) => return collect_leaf_custom_types(&inner.elem),
        Type::Tuple(tuple) => {
            return tuple
                .elems
                .iter()
                .flat_map(collect_leaf_custom_types)
                .collect();
        }
        _ => return Vec::new(),
    };
    let Some(segment) = type_path.path.segments.last() else {
        return Vec::new();
    };

    // Compare on the `syn::Ident` directly (PartialEq<str>) so this
    // hot per-field leaf walker doesn't allocate an owned `String` per
    // call — same pattern `is_serde_json_value_leaf` below uses for the
    // `Value` check.
    // `first_generic_type_arg` skips lifetimes, so `Cow<'a, T>` yields `T`.
    if segment.ident == "Vec"
        || segment.ident == "VecDeque"
        || segment.ident == "LinkedList"
        || segment.ident == "Option"
        || segment.ident == "Box"
        || segment.ident == "Arc"
        || segment.ident == "Rc"
        || segment.ident == "Cow"
        || segment.ident == "NonZero"
        || segment.ident == "HashSet"
        || segment.ident == "BTreeSet"
    {
        return first_generic_type_arg(segment)
            .map(collect_leaf_custom_types)
            .unwrap_or_default();
    }
    if segment.ident == "HashMap" || segment.ident == "BTreeMap" {
        return second_generic_type_arg(segment)
            .map(collect_leaf_custom_types)
            .unwrap_or_default();
    }
    vec![(ty.clone(), segment.ident.to_string())]
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

/// `true` when `ty` (anywhere inside it: nested `<...>` arguments, array
/// lengths, references, tuples) names one of the current item's generic
/// parameters (`T`, a const `N`, a lifetime `'a` — stored as `"'a"`) or
/// `Self`.  The assertions are module-level `const _` items, where none of
/// those is in scope (E0412 / E0425 / E0261 / E0411), and the marker impl is
/// unconditional (`impl<T> Schema for Foo<T> {}`), so such a leaf can only be
/// checked at its use site — it is skipped.
fn type_contains_generic(ty: &Type, generic_idents: &HashSet<String>) -> bool {
    struct Mentions<'a> {
        names: &'a HashSet<String>,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Mentions<'_> {
        // Only the LEADING segment of a path can name a generic parameter
        // (`T`, `T::Assoc`, `Self::X`): a later segment is a module or item
        // (`models::User` is not the generic `User` of `struct Page<User>`).
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(first) = path.segments.first() {
                self.found |=
                    first.ident == "Self" || self.names.contains(&first.ident.to_string());
            }
            syn::visit::visit_path(self, path);
        }
        fn visit_lifetime(&mut self, lifetime: &'ast syn::Lifetime) {
            self.found |= self.names.contains(&format!("'{}", lifetime.ident));
        }
    }
    let mut mentions = Mentions {
        names: generic_idents,
        found: false,
    };
    mentions.visit_type(ty);
    mentions.found
}

/// Whether `ty` is a path into a user module (`api::Header`, `crate::Date`,
/// `super::Uuid`) rather than a bare name or a path into a crate the builtin
/// types come from.  The decision is by origin, not by a list of crate names:
/// a multi-segment path is the user's when its root is `crate` / `self` /
/// `super`, or a name that is not an `--extern` crate of this compilation
/// (`api` in `api::Header` is a local module).  `std` / `core` / `alloc` are
/// never user paths, and neither is a path rooted at a dependency the compiler
/// links (`url::Url`, `http::StatusCode`, `tempfile::NamedTempFile`).
fn is_user_module_path(ty: &Type) -> bool {
    // Leaves reaching here are always paths (`collect_leaf_custom_types`), but
    // a non-path shape is still answered rather than trusted.
    let Type::Path(type_path) = ty else {
        return false;
    };
    let segments = &type_path.path.segments;
    if segments.len() < 2 {
        return false;
    }
    match segments[0].ident.to_string().as_str() {
        "crate" | "self" | "super" => true,
        "std" | "core" | "alloc" => false,
        root => !is_extern_crate(root),
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
    // SeaORM's `JsonValue` alias of `serde_json::Value` (bare or
    // `sea_orm::JsonValue`); its `Json` alias is already a builtin ident. A
    // `JsonValue` under another path is a user type, not the alias.
    if last.ident == "JsonValue" {
        return segments.len() == 1
            || segments
                .iter()
                .any(|segment| segment.ident == "sea_orm" || segment.ident == "serde_json");
    }
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
mod tests;
