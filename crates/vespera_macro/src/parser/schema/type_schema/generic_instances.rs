//! Named components for recursive generic instantiations.
//!
//! A concrete generic type such as `Page<User>` is inlined (its parameters
//! substituted), because OpenAPI has no generics. Inlining a RECURSIVE
//! generic (`struct Tree<T> { value: T, children: Vec<Tree<T>> }`) would never
//! terminate, so the instantiation currently being inlined is tracked: when it
//! is reached again from inside itself, the inner occurrence becomes a `$ref`
//! and the outer one is materialised as a component of its own.
//!
//! The component name is the base schema name followed by every type
//! argument, flattened and joined with `_`: `Tree<i32>` → `Tree_i32`,
//! `Tree<Vec<User>>` → `Tree_Vec_User`, `Pair<A, Option<B>>` →
//! `Pair_A_Option_B` (see [`instance_name`] for how `_` in identifiers,
//! arrays, slices and tuples are spelled so that different types never share
//! a name). Non-recursive generics stay inline.
//!
//! The state is thread-local. `parallel_filter_map` workers hand their
//! materialised components back to the calling thread
//! ([`take_materialized`] / [`merge_materialized`]), and the document
//! generator drains them into `components.schemas` at the end.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
};

use syn::Type;
use vespera_core::schema::{Reference, Schema, SchemaRef};

use crate::parser::schema::substitute_type;

thread_local! {
    /// Instantiation names currently being inlined (outermost first).
    static IN_PROGRESS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Instantiations that were reached from inside themselves.
    static CYCLIC: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Generic definitions being built: `(instance name, component)`, e.g.
    /// `("Tree_T", "Tree")` while the `Tree<T>` component itself is built.
    static DEFINITIONS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// Materialised components, by name.
    static MATERIALIZED: RefCell<BTreeMap<String, Schema>> = const { RefCell::new(BTreeMap::new()) };
}

/// The component name of `base<args..>`.
pub fn instance_name<'a>(base: &str, args: impl IntoIterator<Item = &'a Type>) -> String {
    instance_name_with(base, args, &|_| None)
}

/// [`instance_name`] where a path argument `component` resolves to a
/// component name of its own (`a::Foo` / `b::Foo` renamed apart) is spelled
/// by that name instead of its last ident.
pub fn instance_name_with<'a>(
    base: &str,
    args: impl IntoIterator<Item = &'a Type>,
    component: &dyn Fn(&syn::Path) -> Option<String>,
) -> String {
    let mut name = escape_name(base);
    for arg in args {
        push_type_segments(&mut name, arg, component);
    }
    name
}

/// `name` (an identifier or a component name) spelled without `_`, so it
/// never reads as a separator: `_` → `-`, and the `-` / `.` a
/// `#[schema(name)]` may hold → `.h` / `.d` (a `.` always starts an escape,
/// so `My_Tree` and `My-Tree` stay apart). A Rust identifier only changes
/// its `_`.
fn escape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '_' => out.push('-'),
            '-' => out.push_str(".h"),
            '.' => out.push_str(".d"),
            _ => out.push(ch),
        }
    }
    out
}

/// Append `_<segment>` for every name inside `ty` (prefix order: a generic
/// segment's arguments follow it).
///
/// The encoding is injective for a fixed arity per type name: a name is
/// escaped by [`escape_name`] (`my_type` → `my-type`, so it never reads as
/// a separator; the base name too), and the structural markers carry a `.` (which no
/// identifier contains) together with their size — `[T; 4]` → `_Array.4_T`,
/// `[T]` → `_Slice._T`, `(A, B)` → `_Tuple.2_A_B`, anything else (`impl
/// Trait`, `dyn Trait`, …) → `_.Value`.
fn push_type_segments(
    name: &mut String,
    ty: &Type,
    component: &dyn Fn(&syn::Path) -> Option<String>,
) {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                if let Some(qself) = &type_path.qself {
                    // `<T as Tr>::Out`: the self type leads the spelling.
                    name.push_str("_.Qself");
                    push_type_segments(name, &qself.ty, component);
                }
                name.push('_');
                name.push_str(&path_spelling(&type_path.path, component));
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    for arg in &args.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            push_type_segments(name, inner, component);
                        }
                    }
                }
            }
        }
        Type::Reference(reference) => push_type_segments(name, &reference.elem, component),
        Type::Paren(paren) => push_type_segments(name, &paren.elem, component),
        Type::Group(group) => push_type_segments(name, &group.elem, component),
        Type::Array(array) => {
            name.push_str("_Array.");
            push_array_len(name, &array.len);
            push_type_segments(name, &array.elem, component);
        }
        Type::Slice(slice) => {
            name.push_str("_Slice.");
            push_type_segments(name, &slice.elem, component);
        }
        Type::Tuple(tuple) => {
            name.push_str("_Tuple.");
            name.push_str(&tuple.elems.len().to_string());
            for elem in &tuple.elems {
                push_type_segments(name, elem, component);
            }
        }
        _ => name.push_str("_.Value"),
    }
}

/// The spelling of a type path: its registered component name, else every
/// segment (`other::Foo` → `other.pFoo`, `.p` standing for `::`), so a
/// module-qualified type never shares the bare type's name.
fn path_spelling(path: &syn::Path, component: &dyn Fn(&syn::Path) -> Option<String>) -> String {
    if let Some(name) = component(path) {
        return escape_name(&name);
    }
    // A leading `crate::` names the same items as the bare path (`crate::User`
    // is `User`), so it is not spelled.
    let skip = usize::from(
        path.leading_colon.is_none()
            && path.segments.len() > 1
            && path.segments[0].ident == "crate",
    );
    path.segments
        .iter()
        .skip(skip)
        .map(|segment| escape_name(&segment.ident.to_string()))
        .collect::<Vec<_>>()
        .join(".p")
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The length of `[T; len]`: an integer literal as written (its value), a
/// const path as its (qualified, escaped) spelling, and any other expression
/// as the hex bytes of its tokens (`.e`), so distinct lengths stay distinct.
fn push_array_len(name: &mut String, len: &syn::Expr) {
    match len {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) => name.push_str(int.base10_digits()),
        syn::Expr::Path(path) => name.push_str(&path_spelling(&path.path, &|_| None)),
        other => {
            name.push_str(".e");
            let tokens = quote::ToTokens::to_token_stream(other).to_string();
            for byte in tokens.bytes() {
                name.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
                name.push(char::from(HEX_DIGITS[usize::from(byte & 0xf)]));
            }
        }
    }
}

/// Inline the generic instantiation `name`, built by `build`, unless that
/// would recurse forever — then a `$ref` to the materialised component.
pub fn inline_or_reference(name: &str, build: impl FnOnce() -> Schema) -> SchemaRef {
    // In the request view the instantiation carries a `.Input` suffix when
    // the definition has an input view; it then refers to that input.
    let request = super::super::request_view::in_request_view();
    let definition = DEFINITIONS.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find(|(instance, _)| {
                instance == name
                    || (request && name.strip_suffix(".Input") == Some(instance.as_str()))
            })
            .map(|(_, component)| component.clone())
    });
    if let Some(component) = definition {
        let component =
            super::super::request_view::input_component(&component).unwrap_or(component);
        return SchemaRef::Ref(Reference::schema(&component));
    }
    if MATERIALIZED.with(|map| map.borrow().contains_key(name)) {
        return SchemaRef::Ref(Reference::schema(name));
    }
    let recursive = IN_PROGRESS.with(|stack| stack.borrow().iter().any(|open| open == name));
    if recursive {
        CYCLIC.with(|cyclic| cyclic.borrow_mut().insert(name.to_string()));
        return SchemaRef::Ref(Reference::schema(name));
    }
    IN_PROGRESS.with(|stack| stack.borrow_mut().push(name.to_string()));
    let schema = build();
    IN_PROGRESS.with(|stack| stack.borrow_mut().pop());
    if CYCLIC.with(|cyclic| cyclic.borrow_mut().remove(name)) {
        MATERIALIZED.with(|map| map.borrow_mut().insert(name.to_string(), schema));
        SchemaRef::Ref(Reference::schema(name))
    } else {
        SchemaRef::Inline(Box::new(schema))
    }
}

/// Run `build` (the schema of the struct / enum definition `ident` with
/// `generics` and container `attrs`); for a generic definition such as
/// `Tree<T>`, its own instantiation `Tree<T>` inside it refers back to its
/// component (`Tree`) instead of materialising `Tree_T`.
pub fn with_definition<R>(
    ident: &syn::Ident,
    generics: &syn::Generics,
    attrs: &[syn::Attribute],
    build: impl FnOnce() -> R,
) -> R {
    let params: Vec<Type> = generics
        .type_params()
        .map(|param| {
            let ident = &param.ident;
            syn::parse_quote!(#ident)
        })
        .collect();
    if params.is_empty() {
        return build();
    }
    let component =
        crate::schema_impl::extract_schema_name_attr(attrs).unwrap_or_else(|| ident.to_string());
    // A self-reference may omit a defaulted parameter (`Tree<T>` inside
    // `Tree<T, M = Meta>` means `Tree<T, Meta>`): it names the same component
    // as the parameters spelled out (`Tree<T, M>`).
    let as_written = instance_name(&component, &params);
    let defaults_filled = instance_name(&component, &default_filled_params(generics, &params));
    let instances = [as_written, defaults_filled];
    with_self_instances(&instances, &component, build)
}

/// `params` (the type parameters as types) with every defaulted parameter
/// replaced by its default, the earlier parameters substituted into it
/// (`Tree<T, M = Meta>` → `[T, Meta]`).
fn default_filled_params(generics: &syn::Generics, params: &[Type]) -> Vec<Type> {
    let names: Vec<String> = generics
        .type_params()
        .map(|param| param.ident.to_string())
        .collect();
    let mut filled: Vec<Type> = Vec::with_capacity(params.len());
    for (index, (param, own)) in generics.type_params().zip(params).enumerate() {
        let ty = match &param.default {
            Some((_, default)) => {
                let earlier: Vec<&Type> = filled.iter().collect();
                substitute_type(default, &names[..index], &earlier)
            }
            None => own.clone(),
        };
        filled.push(ty);
    }
    filled
}

/// Run `build` (the schema of a generic definition such as `Tree<T>`) with
/// its own instantiations `instances` (`Tree_T`, ...) referring to `component`.
pub fn with_self_instances<R>(
    instances: &[String],
    component: &str,
    build: impl FnOnce() -> R,
) -> R {
    let depth = DEFINITIONS.with(|stack| stack.borrow().len());
    DEFINITIONS.with(|stack| {
        stack.borrow_mut().extend(
            instances
                .iter()
                .map(|instance| (instance.clone(), component.to_string())),
        );
    });
    let result = build();
    DEFINITIONS.with(|stack| stack.borrow_mut().truncate(depth));
    result
}

/// The component materialised on this thread under `name`, if any.
pub fn materialized(name: &str) -> Option<Schema> {
    MATERIALIZED.with(|map| map.borrow().get(name).cloned())
}

/// Remove and return every component materialised on this thread.
pub fn take_materialized() -> BTreeMap<String, Schema> {
    MATERIALIZED.with(|map| std::mem::take(&mut *map.borrow_mut()))
}

/// Move every component materialised on this thread into `schemas`. A name
/// a user component already holds (`#[schema(name = "Tree_i32")]` next to a
/// recursive `Tree<i32>`) is an error: every `$ref` to it would resolve to
/// the user's schema and the instance would be silently dropped.
pub fn drain_materialized_into(schemas: &mut BTreeMap<String, Schema>) -> syn::Result<()> {
    for (name, schema) in take_materialized() {
        if schemas.contains_key(&name) {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "vespera! macro: the component `{name}` of a recursive generic instantiation collides with a schema of the same name. Rename that schema (`#[schema(name = \"..\")]`)."
                ),
            ));
        }
        schemas.insert(name, schema);
    }
    Ok(())
}

/// Add components materialised on another thread to this thread's set.
pub fn merge_materialized(components: BTreeMap<String, Schema>) {
    MATERIALIZED.with(|map| {
        let mut map = map.borrow_mut();
        for (name, schema) in components {
            map.entry(name).or_insert(schema);
        }
    });
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use vespera_core::schema::SchemaType;

    use super::*;

    #[rstest]
    #[case::scalar("Tree", &["i32"], "Tree_i32")]
    #[case::nested("Tree", &["Vec<crate::User>"], "Tree_Vec_User")]
    #[case::two_args("Pair", &["A", "Option<B>"], "Pair_A_Option_B")]
    #[case::reference("Page", &["&'static str"], "Page_str")]
    #[case::paren("Page", &["(u8)"], "Page_u8")]
    #[case::array_slice("Page", &["[u8; 4]", "&[u8]"], "Page_Array.4_u8_Slice._u8")]
    #[case::array_const_len("Page", &["[u8; LEN_MAX]"], "Page_Array.LEN-MAX_u8")]
    #[case::array_expr_len("Page", &["[u8; 2 * 2]"], "Page_Array..e32202a2032_u8")]
    #[case::array_path_len("Page", &["[u8; crate::N]"], "Page_Array.N_u8")]
    #[case::module_qualified("Page", &["other::Foo"], "Page_other.pFoo")]
    #[case::crate_qualified("Page", &["crate::Foo"], "Page_Foo")]
    #[case::tuples("Page", &["()", "(u8, bool)"], "Page_Tuple.0_Tuple.2_u8_bool")]
    #[case::other("Page", &["impl Trait"], "Page_.Value")]
    #[case::qualified_self("Page", &["<T as Tr>::Out"], "Page_.Qself_T_Tr.pOut")]
    #[case::underscore_ident("Pair", &["A_B", "C"], "Pair_A-B_C")]
    // Round 11: the base is escaped like an argument.
    #[case::underscore_base("My_Tree", &["i32"], "My-Tree_i32")]
    #[case::dash_dot_base("My-Tree.v1", &["i32"], "My.hTree.dv1_i32")]
    fn instance_names(#[case] base: &str, #[case] args: &[&str], #[case] expected: &str) {
        let types: Vec<Type> = args
            .iter()
            .map(|arg| syn::parse_str(arg).unwrap())
            .collect();
        assert_eq!(instance_name(base, &types), expected);
    }

    /// Distinct argument lists never share a component name (each pair
    /// collided before: `_` inside identifiers, array lengths, tuple arity,
    /// array vs slice).
    #[rstest]
    #[case::underscore(&["A_B", "C"], &["A", "B_C"])]
    #[case::array_len(&["[u8; 4]"], &["[u8; 8]"])]
    #[case::array_vs_slice(&["[u8; 4]"], &["&[u8]"])]
    #[case::tuple_arity(&["(A, B)", "(C,)"], &["(A,)", "(B, C)"])]
    #[case::marker_vs_ident(&["[u8]"], &["Array<u8>"])]
    // A module-qualified type and a qualified self type are not their bare
    // names; a const path and an expression length are not the same length.
    #[case::qualified_vs_bare(&["other::Foo"], &["Foo"])]
    #[case::qualified_self_vs_bare(&["<T as Tr>::Out"], &["Out"])]
    #[case::const_len_vs_expr(&["[u8; N]"], &["[u8; 2 * 2]"])]
    #[case::const_len_path_vs_bare(&["[u8; a::N]"], &["[u8; N]"])]
    fn instance_names_are_injective(#[case] left: &[&str], #[case] right: &[&str]) {
        let parse = |args: &[&str]| -> Vec<Type> {
            args.iter()
                .map(|arg| syn::parse_str(arg).unwrap())
                .collect()
        };
        assert_ne!(
            instance_name("Pair", &parse(left)),
            instance_name("Pair", &parse(right))
        );
    }

    #[test]
    fn base_names_never_collide_with_arguments() {
        let i32_ty: Type = syn::parse_str("i32").unwrap();
        let tree_i32: Type = syn::parse_str("Tree<i32>").unwrap();
        assert_ne!(
            instance_name("My_Tree", [&i32_ty]),
            instance_name("My", [&tree_i32])
        );
        assert_ne!(
            instance_name("My_Tree", [&i32_ty]),
            instance_name("My-Tree", [&i32_ty])
        );
    }

    #[test]
    fn group_type_is_transparent() {
        let group = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(syn::parse_str("u8").unwrap()),
        });
        assert_eq!(instance_name("Page", [&group]), "Page_u8");
    }

    #[test]
    fn recursion_is_materialised_and_reused() {
        take_materialized();
        let object = || Schema::new(SchemaType::Object);
        let outer = inline_or_reference("Tree_i32", || {
            let inner = inline_or_reference("Tree_i32", object);
            assert!(matches!(inner, SchemaRef::Ref(_)));
            object()
        });
        assert!(
            matches!(outer, SchemaRef::Ref(ref r) if r.ref_path == "#/components/schemas/Tree_i32")
        );
        // Already materialised: referenced without rebuilding.
        let again = inline_or_reference("Tree_i32", || unreachable!());
        assert!(matches!(again, SchemaRef::Ref(_)));
        // Non-recursive instantiations stay inline.
        assert!(matches!(
            inline_or_reference("Page_u8", object),
            SchemaRef::Inline(_)
        ));

        let drained = take_materialized();
        assert_eq!(drained.keys().collect::<Vec<_>>(), ["Tree_i32"]);
        merge_materialized(drained.clone());
        merge_materialized(drained);
        assert_eq!(take_materialized().len(), 1);
    }
}
