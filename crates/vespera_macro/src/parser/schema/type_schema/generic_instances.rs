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
//! `Pair_A_Option_B`. Non-recursive generics stay inline.
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
    let mut name = base.to_string();
    for arg in args {
        push_type_segments(&mut name, arg);
    }
    name
}

/// Append `_<segment>` for every name inside `ty`.
fn push_type_segments(name: &mut String, ty: &Type) {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                name.push('_');
                name.push_str(&segment.ident.to_string());
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    for arg in &args.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            push_type_segments(name, inner);
                        }
                    }
                }
            }
        }
        Type::Reference(reference) => push_type_segments(name, &reference.elem),
        Type::Paren(paren) => push_type_segments(name, &paren.elem),
        Type::Group(group) => push_type_segments(name, &group.elem),
        Type::Array(array) => {
            name.push_str("_Array");
            push_type_segments(name, &array.elem);
        }
        Type::Slice(slice) => {
            name.push_str("_Array");
            push_type_segments(name, &slice.elem);
        }
        Type::Tuple(tuple) if tuple.elems.is_empty() => name.push_str("_Unit"),
        Type::Tuple(tuple) => {
            name.push_str("_Tuple");
            for elem in &tuple.elems {
                push_type_segments(name, elem);
            }
        }
        _ => name.push_str("_Value"),
    }
}

/// Inline the generic instantiation `name`, built by `build`, unless that
/// would recurse forever — then a `$ref` to the materialised component.
pub fn inline_or_reference(name: &str, build: impl FnOnce() -> Schema) -> SchemaRef {
    let definition = DEFINITIONS.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find(|(instance, _)| instance == name)
            .map(|(_, component)| component.clone())
    });
    if let Some(component) = definition {
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

/// Run `build` (the schema of a generic definition such as `Tree<T>`) with
/// its own instantiation `instance` (`Tree_T`) referring to `component`.
pub fn with_self_instance<R>(instance: String, component: String, build: impl FnOnce() -> R) -> R {
    DEFINITIONS.with(|stack| stack.borrow_mut().push((instance, component)));
    let result = build();
    DEFINITIONS.with(|stack| stack.borrow_mut().pop());
    result
}

/// Remove and return every component materialised on this thread.
pub fn take_materialized() -> BTreeMap<String, Schema> {
    MATERIALIZED.with(|map| std::mem::take(&mut *map.borrow_mut()))
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
    #[case::array_slice("Page", &["[u8; 4]", "&[u8]"], "Page_Array_u8_Array_u8")]
    #[case::tuples("Page", &["()", "(u8, bool)"], "Page_Unit_Tuple_u8_bool")]
    #[case::other("Page", &["impl Trait"], "Page_Value")]
    #[case::qualified_self("Page", &["<T as Tr>::Out"], "Page_Out")]
    fn instance_names(#[case] base: &str, #[case] args: &[&str], #[case] expected: &str) {
        let types: Vec<Type> = args
            .iter()
            .map(|arg| syn::parse_str(arg).unwrap())
            .collect();
        assert_eq!(instance_name(base, &types), expected);
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
