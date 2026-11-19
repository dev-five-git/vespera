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
use syn::visit::Visit;
use syn::{GenericArgument, PathArguments, Type};

use crate::parser::schema::{
    SerdeSide, extract_field_codec, extract_skip, is_builtin_openapi_type,
    schema_attrs::SchemaConstraints, wire_override::container_override_type,
};

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
        if type_contains_generic(&leaf, generic_idents)
            || builtin
            || is_serde_json_value_leaf(&leaf)
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
/// `super::Uuid`) rather than a bare name or one of the external crates the
/// builtin types come from (`chrono::`, `rust_decimal::`, `axum::`, ...).
fn is_user_module_path(ty: &Type) -> bool {
    const EXTERNAL_ROOTS: [&str; 11] = [
        "axum",
        "alloc",
        "chrono",
        "core",
        "rust_decimal",
        "sea_orm",
        "serde_json",
        "std",
        "time",
        "uuid",
        "vespera",
    ];
    // Leaves reaching here are always paths (`collect_leaf_custom_types`), so
    // a non-path type needs no arm of its own.
    matches!(ty, Type::Path(type_path) if type_path.path.segments.len() > 1
        && !EXTERNAL_ROOTS.iter().any(|root| type_path.path.segments[0].ident == *root))
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
            .map(|(ty, _)| quote!(#ty).to_string().replace(' ', ""))
            .collect()
    }

    fn empty_path_type() -> Type {
        Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        })
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

    /// Smart pointers / extra std collections are transparent like `Box`/`Vec`.
    #[rstest::rstest]
    #[case::arc("Arc<MyType>")]
    #[case::rc("std::rc::Rc<MyType>")]
    #[case::cow("Cow<'static, MyType>")]
    #[case::vec_deque("VecDeque<MyType>")]
    #[case::linked_list("LinkedList<MyType>")]
    #[case::nested("Arc<Vec<Cow<'a, MyType>>>")]
    fn leaf_of_smart_pointers_and_collections_unwraps_inner(#[case] src: &str) {
        assert_eq!(leaves_of(src), vec!["MyType"]);
    }

    #[rstest::rstest]
    #[case::non_zero_u32("NonZeroU32")]
    #[case::non_zero_generic("NonZero<u64>")]
    #[case::cow_str("Cow<'a, str>")]
    #[case::arc_string("Arc<String>")]
    #[case::ip_addr("std::net::IpAddr")]
    #[case::ipv4("Ipv4Addr")]
    #[case::ipv6("Option<Ipv6Addr>")]
    #[case::socket_addr("Vec<SocketAddr>")]
    #[case::socket_addr_v4("SocketAddrV4")]
    #[case::socket_addr_v6("SocketAddrV6")]
    #[case::path_buf("std::path::PathBuf")]
    #[case::url("Option<url::Url>")]
    #[case::duration("std::time::Duration")]
    fn builtin_wrapped_leaves_need_no_assertion(#[case] src: &str) {
        for (_, ident) in collect_leaf_custom_types(&parse_ty(src)) {
            assert!(is_builtin_openapi_type(&ident), "{src}: {ident}");
        }
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

    /// References, slices, arrays, tuples, parentheses and `$t:ty` groups
    /// are documented through their elements, so their custom leaves are
    /// asserted too.
    #[rstest::rstest]
    #[case::tuple("(MyType, u32)", &["MyType", "u32"])]
    #[case::reference("&'a MyType", &["MyType"])]
    #[case::slice("&[MyType]", &["MyType"])]
    #[case::array("[Vec<MyType>; 2]", &["MyType"])]
    #[case::paren("(MyType)", &["MyType"])]
    #[case::unit("()", &[])]
    #[case::fn_pointer("fn(MyType)", &[])]
    fn leaf_of_structural_types(#[case] src: &str, #[case] expected: &[&str]) {
        assert_eq!(leaves_of(src), expected);
    }

    #[test]
    fn leaf_of_macro_type_group() {
        let group = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(parse_ty("Option<MyType>")),
        });
        let leaves: Vec<String> = collect_leaf_custom_types(&group)
            .into_iter()
            .map(|(_, ident)| ident)
            .collect();
        assert_eq!(leaves, ["MyType"]);
    }

    #[test]
    fn leaf_of_empty_wrapper_is_empty() {
        // `Vec` with no type args (degenerate) has no leaf.
        assert_eq!(
            collect_leaf_custom_types(&parse_ty("Vec")),
            Vec::<(Type, String)>::new()
        );
    }

    #[test]
    fn leaf_of_empty_path_is_empty() {
        assert_eq!(
            collect_leaf_custom_types(&empty_path_type()),
            Vec::<(Type, String)>::new()
        );
    }

    #[test]
    fn generic_argument_helpers_ignore_lifetimes_and_missing_arguments() {
        let vec_lifetime = parse_ty("Vec<'a>");
        let Type::Path(vec_path) = vec_lifetime else {
            panic!("fixture must be a path type");
        };
        let vec_segment = vec_path.path.segments.last().expect("Vec segment");

        let bare_map = parse_ty("HashMap");
        let Type::Path(bare_map_path) = bare_map else {
            panic!("fixture must be a path type");
        };
        let bare_map_segment = bare_map_path.path.segments.last().expect("HashMap segment");

        let lifetime_map = parse_ty("HashMap<'a, 'b>");
        let Type::Path(lifetime_map_path) = lifetime_map else {
            panic!("fixture must be a path type");
        };
        let lifetime_map_segment = lifetime_map_path
            .path
            .segments
            .last()
            .expect("HashMap segment");

        assert_eq!(first_generic_type_arg(vec_segment), None);
        assert_eq!(second_generic_type_arg(bare_map_segment), None);
        assert_eq!(second_generic_type_arg(lifetime_map_segment), None);
    }

    #[test]
    fn leaf_pairs_carry_the_final_path_ident() {
        let leaves = collect_leaf_custom_types(&parse_ty("Vec<Option<crate::models::User>>"));
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].1, "User");
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

    #[test]
    fn serde_json_allowlist_rejects_non_path_and_empty_path_types() {
        assert!(!is_serde_json_value_leaf(&parse_ty("&Value")));
        assert!(!is_serde_json_value_leaf(&empty_path_type()));
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

    /// A generic named like a later segment of a qualified path does not
    /// shadow the path (`models::User` under `struct Page<User>` is still
    /// asserted), while a generic leading a path (`T::Item`) is skipped.
    #[test]
    fn qualified_path_is_not_shadowed_by_a_later_generic_segment() {
        let mut generics = HashSet::new();
        generics.insert("User".to_string());
        assert!(!type_contains_generic(&parse_ty("models::User"), &generics));
        assert!(!type_contains_generic(
            &parse_ty("::models::User"),
            &generics
        ));
        assert!(type_contains_generic(&parse_ty("Vec<User>"), &generics));
        assert!(type_contains_generic(&parse_ty("User::Item"), &generics));
        assert!(type_contains_generic(&parse_ty("Self"), &generics));
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

    #[test]
    fn generic_detection_descends_through_references_and_rejects_other_types() {
        let generics = HashSet::from(["T".to_string()]);

        assert!(type_contains_generic(&parse_ty("&T"), &generics));
        assert!(type_contains_generic(&parse_ty("(T, String)"), &generics));
        assert!(!type_contains_generic(&parse_ty("&MyType"), &generics));
        assert!(!type_contains_generic(&parse_ty("(u8, String)"), &generics));
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

    /// Leaves no module-level `const _` can name (a lifetime / const
    /// parameter, `Self`) and fully skipped fields emit nothing.
    #[rstest::rstest]
    #[case::serde_skip(parse_quote!(struct S { #[serde(skip)] pool: PgPool }))]
    #[case::skip_both_sides(parse_quote!(struct S {
        #[serde(skip_serializing)]
        #[serde(skip_deserializing)]
        pool: PgPool,
    }))]
    #[case::lifetime(parse_quote!(struct S<'a> { name: Name<'a> }))]
    #[case::const_param(parse_quote!(struct S<const N: usize> { items: Items<N> }))]
    #[case::self_type(parse_quote!(struct Node { children: Vec<Box<Self>> }))]
    fn no_assertion_for_unnameable_or_skipped_leaf(#[case] input: syn::DeriveInput) {
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(out.is_empty(), "expected no assertion, got: {out}");
    }

    #[test]
    fn one_sided_skip_still_asserts() {
        let input: syn::DeriveInput = parse_quote! {
            struct S<'a> { #[serde(skip_serializing)] pool: PgPool, other: Name<'static> }
        };
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        assert!(out.contains("PgPool") && out.contains("Name"), "{out}");
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
        assert_eq!(
            emit_field_assertions(&input, &constraints_for(&input)).to_string(),
            ""
        );

        let input: syn::DeriveInput = parse_quote! {
            pub struct Tuple(MyUnknown);
        };
        assert_eq!(
            emit_field_assertions(&input, &constraints_for(&input)).to_string(),
            ""
        );

        let input: syn::DeriveInput = parse_quote! {
            pub struct Unit;
        };
        assert_eq!(
            emit_field_assertions(&input, &constraints_for(&input)).to_string(),
            ""
        );
    }

    /// The leaf types asserted by `emit_field_assertions`, in emission order.
    fn asserted_leaves(src: &str) -> Vec<String> {
        let input: syn::DeriveInput = syn::parse_str(src).unwrap();
        let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
        out.split("let _ = __vespera_assert_schema :: < ")
            .skip(1)
            .map(|rest| rest.split(" > ;").next().unwrap().replace(' ', ""))
            .collect()
    }

    #[rstest::rstest]
    #[case::json_value_aliases(
        "struct S { a: JsonValue, b: sea_orm::JsonValue, c: Option<Json>, d: sea_orm::prelude::Json }",
        &[]
    )]
    #[case::field_value_type_replaces_field_type(
        "struct S { #[schema(value_type = Vec<Wire>)] a: Opaque, #[schema(value_type = i64)] b: Opaque }",
        &["Wire"]
    )]
    #[case::with_without_value_type_is_skipped(
        r#"struct S { #[serde(with = "m")] a: Opaque, #[serde(serialize_with = "f", deserialize_with = "g")] b: Opaque }"#,
        &[]
    )]
    #[case::one_sided_codec_keeps_the_field_type(
        r#"struct S { #[serde(serialize_with = "f")] a: Opaque }"#,
        &["Opaque"]
    )]
    #[case::container_into_keeps_fields(
        r#"#[serde(into = "Wire")] struct S { a: Opaque }"#,
        &["Wire", "Opaque"]
    )]
    #[case::container_both_sides_skip_fields(
        r#"#[serde(into = "Wire", try_from = "Raw")] struct S { a: Opaque }"#,
        &["Wire", "Raw"]
    )]
    #[case::container_value_type_skips_fields(
        "#[schema(value_type = Vec<Wire>)] struct S { a: Opaque }",
        &["Wire", "Wire"]
    )]
    #[case::enum_conversion(
        r#"#[serde(into = "String", from = "Raw")] enum E { A(Opaque) }"#,
        &["Raw"]
    )]
    #[case::generic_conversion_is_skipped(r#"#[serde(into = "Vec<T>")] struct S<T> { a: T }"#, &[])]
    fn wire_override_assertions(#[case] src: &str, #[case] expected: &[&str]) {
        assert_eq!(asserted_leaves(src), expected);
    }

    /// A builtin-named type under a user module is checked, so a forgotten
    /// derive there cannot document as the builtin. Bare names and the
    /// external crates' own paths keep their builtin exemption.
    #[rstest::rstest]
    #[case::user_module_builtin_name("struct S { a: api::Header }", &["api::Header"])]
    #[case::crate_path_builtin_name("struct S { a: crate::models::Date }", &["crate::models::Date"])]
    #[case::external_builtin_names(
        "struct S { a: chrono::DateTime<Utc>, b: axum::Json<u8>, c: rust_decimal::Decimal, d: vespera::Json<u8> }",
        &[]
    )]
    #[case::bare_builtin_names("struct S { a: Header, b: Decimal }", &[])]
    #[case::user_json_value_alias("struct S { a: models::JsonValue }", &["models::JsonValue"])]
    #[case::external_json_value_aliases("struct S { a: sea_orm::JsonValue, b: serde_json::Value }", &[])]
    fn builtin_named_leaves_follow_their_path(#[case] src: &str, #[case] expected: &[&str]) {
        assert_eq!(asserted_leaves(src), expected);
    }
}
