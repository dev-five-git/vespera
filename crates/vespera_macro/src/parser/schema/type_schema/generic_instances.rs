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
//! Polymorphic recursion (`struct Nested<T> { a: Vec<Nested<Vec<T>>>, .. }`)
//! never repeats an instance, so it cannot be cut by the `$ref` above. Its
//! expansion would fan out with every level, so the instances built for one
//! top-level type are capped at [`MAX_INSTANCES_PER_ROOT`]; past the cap an
//! instance is documented as any value (`{}`), like an unknown type.
//!
//! The state is thread-local. `parallel_filter_map` workers hand their
//! materialised components back to the calling thread
//! ([`take_materialized`] / [`merge_materialized`]), and the document
//! generator drains them into `components.schemas` at the end.

use std::{
    cell::{Cell, RefCell},
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
    /// Instances built since the current top-level instance started.
    static BUILT: Cell<usize> = const { Cell::new(0) };
}

/// How many generic instances one top-level instance may build inline before
/// a polymorphically recursive expansion is cut (see the module docs).
const MAX_INSTANCES_PER_ROOT: usize = 4096;

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

/// Appends the const arguments of an instantiation (in parameter order) to
/// `name`, spelled as an array length is with a `_Const.` marker: `Buf<u8, 4>`
/// and `Buf<u8, 8>` never share a component name.
pub fn push_const_segments<'a>(name: &mut String, consts: impl IntoIterator<Item = &'a syn::Expr>) {
    for expr in consts {
        name.push_str("_Const.");
        push_array_len(name, expr);
    }
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
                        } else if let syn::GenericArgument::Const(expr) = arg {
                            // A nested const argument (`Tree<Buf<u8, 4>>`) is
                            // spelled as a top-level one is
                            // ([`push_const_segments`]), so `Buf<u8, 4>` and
                            // `Buf<u8, 8>` never share a name. A bare
                            // identifier (`Buf<u8, LEN>`) parses as a type and
                            // spells as one; a const slot never holds a type,
                            // so that cannot collide with a type argument.
                            name.push_str("_Const.");
                            push_array_len(name, expr);
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
    if IN_PROGRESS.with(|stack| stack.borrow().is_empty()) {
        BUILT.with(|built| built.set(0));
    }
    let earlier = BUILT.with(|built| built.replace(built.get() + 1));
    if earlier >= MAX_INSTANCES_PER_ROOT {
        return SchemaRef::Inline(Box::default());
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
/// `Tree<T>` (or `Node<const N: usize>`, or `Buf<T, const N: usize>`), its own
/// instantiation inside it (`Tree<T>`, `Node<N>`, `Buf<T, N>`) refers back to
/// its component (`Tree`) instead of materialising `Tree_T` / `Node_Const.N`.
///
/// The names are visible only in this definition's own body. A name is just
/// the spelling of its arguments, so inside ANOTHER definition's body
/// (`Holder<T> { fixed: Buf<T, N> }` built while `Buf` is, with a global
/// `const N`) the same spelling may denote an outer item: those inlined bodies
/// see none of the enclosing definition's names. The limit: a self-reference
/// reached through another definition (`Tree<T>` -> `Wrap<Tree<T>>` -> `Tree<T>`)
/// is materialised as a component of its own instead of a `$ref` to `Tree`.
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
    // A const parameter is spelled by its argument expression (`N`), as a
    // const argument of an instantiation is ([`push_const_segments`]).
    let consts: Vec<syn::Expr> = generics
        .const_params()
        .map(|param| {
            let ident = &param.ident;
            syn::parse_quote!(#ident)
        })
        .collect();
    let component =
        crate::schema_impl::extract_schema_name_attr(attrs).unwrap_or_else(|| ident.to_string());
    // A self-reference may omit a defaulted parameter (`Tree<T>` inside
    // `Tree<T, M = Meta>` means `Tree<T, Meta>`): it names the same component
    // as the parameters spelled out (`Tree<T, M>`). The same holds for a
    // defaulted const (`Buf<T>` inside `Buf<T, const N: usize = 4>`).
    let instances = if params.is_empty() && consts.is_empty() {
        // A non-generic definition has no instance of its own to name.
        Vec::new()
    } else {
        let as_written = definition_instance(&component, &params, &consts);
        let defaults_filled = definition_instance(
            &component,
            &default_filled_params(generics, &params),
            &default_filled_consts(generics),
        );
        vec![as_written, defaults_filled]
    };
    with_self_instances(&instances, &component, build)
}

/// The name of `component` instantiated with the type arguments `types`
/// followed by the const arguments `consts`: the order an instantiation is
/// spelled in (`conversion` appends the const segments after the types).
fn definition_instance(component: &str, types: &[Type], consts: &[syn::Expr]) -> String {
    let mut name = instance_name(component, types);
    push_const_segments(&mut name, consts.iter());
    name
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
/// The enclosing definitions' instances are hidden meanwhile (see
/// [`with_definition`]).
pub fn with_self_instances<R>(
    instances: &[String],
    component: &str,
    build: impl FnOnce() -> R,
) -> R {
    let outer = DEFINITIONS.with(|stack| std::mem::take(&mut *stack.borrow_mut()));
    DEFINITIONS.with(|stack| {
        stack.borrow_mut().extend(
            instances
                .iter()
                .map(|instance| (instance.clone(), component.to_string())),
        );
    });
    let result = build();
    DEFINITIONS.with(|stack| *stack.borrow_mut() = outer);
    result
}

/// The const parameters of `generics`, each defaulted one replaced by its
/// default: the argument an omitted const takes at a use site
/// (`const_argument_exprs`), so `Buf<T>` names `Buf<T, 4>`.
fn default_filled_consts(generics: &syn::Generics) -> Vec<syn::Expr> {
    generics
        .const_params()
        .map(|param| {
            if let Some((_, default)) = &param.default {
                return default.clone();
            }
            let ident = &param.ident;
            syn::parse_quote!(#ident)
        })
        .collect()
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
    // A const argument of a nested type is spelled like a top-level one; a
    // bare identifier in that slot spells as the type it parses as.
    #[case::nested_const_argument("Tree", &["Buf<u8, 4>"], "Tree_Buf_u8_Const.4")]
    #[case::nested_bare_const_argument("Tree", &["Buf<u8, LEN>"], "Tree_Buf_u8_LEN")]
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
    // Round 30: nested const arguments are part of the name, so two
    // instances of a nested const generic never share a component.
    #[case::nested_const_len(&["Tree<Buf<u8, 4>>"], &["Tree<Buf<u8, 8>>"])]
    #[case::nested_const_vs_bare_const(&["Tree<Buf<u8, 4>>"], &["Tree<Buf<u8, LEN>>"])]
    #[case::nested_const_vs_type(&["Tree<Buf<u8, 4>>"], &["Tree<Buf<u8, Four>>"])]
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
