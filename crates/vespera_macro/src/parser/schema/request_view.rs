//! FastAPI-style request views: `{Name}Input` components.
//!
//! A component schema describes what a type SERIALIZES to (the response
//! view). serde can deserialize the same type from a different shape: a
//! `#[serde(default)]` field may be omitted, a `skip_deserializing` field is
//! never read, a `skip_serializing` field is read but never written, and
//! `rename(deserialize = "..")` changes the key. When a type's request view
//! differs from its response view, vespera emits a second component
//! `{Name}Input` and request bodies reference it; the response component is
//! unchanged.
//!
//! A type's request view differs when (directly):
//! - the container or any named field has `#[serde(default)]`,
//! - a field has `skip_deserializing` or `skip_serializing` (but not both),
//! - a field's or the container's `rename(deserialize = ..)` /
//!   `rename_all(deserialize = ..)` differs from the serialize side,
//!
//! or (transitively) when it names a type whose request view differs: a
//! `CreateOrder { items: Vec<LineItem> }` references `LineItemInput` from
//! `CreateOrderInput`.
//!
//! The input component is named `{Name}Input`; when that name is already
//! taken (a real `UserInput` type, or another input), `{Name}Input2`,
//! `{Name}Input3`, ... — the first free one, in component-name order.
//!
//! Like `generic_instances`, the state is thread-local: the document
//! generator installs the input-name map before building schemas, workers
//! receive a copy ([`snapshot`] / [`install`]), and request bodies are built
//! inside [`with_request_view`].

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use super::serde_attrs::{
    SerdeSide, extract_default, extract_field_rename_for, extract_rename_all_for,
    extract_serde_skip_flags,
};

/// Component name → input component name, for every type whose request view
/// differs (Rust-ident aliases of renamed schemas included).
pub type InputNames = Arc<BTreeMap<String, String>>;

thread_local! {
    static REQUEST_VIEW: Cell<bool> = const { Cell::new(false) };
    static INPUT_NAMES: RefCell<InputNames> = RefCell::new(Arc::default());
}

/// Whether schemas are currently being built for a request body.
pub fn in_request_view() -> bool {
    REQUEST_VIEW.with(Cell::get)
}

/// Run `build` producing the request view (restores the previous mode).
pub fn with_request_view<R>(build: impl FnOnce() -> R) -> R {
    let previous = REQUEST_VIEW.with(|mode| mode.replace(true));
    let result = build();
    REQUEST_VIEW.with(|mode| mode.set(previous));
    result
}

/// The input component for `component` while building a request view.
pub fn input_component(component: &str) -> Option<String> {
    in_request_view().then(|| input_name(component)).flatten()
}

/// Whether `component` has an input view (regardless of the current mode).
pub fn has_input(component: &str) -> bool {
    input_name(component).is_some()
}

/// The input component name of `component` (regardless of the current mode).
pub fn input_name(component: &str) -> Option<String> {
    INPUT_NAMES.with(|names| names.borrow().get(component).cloned())
}

/// Whether any type named inside `types` has an input view.
pub fn mentions_input<'a>(types: impl IntoIterator<Item = &'a syn::Type>) -> bool {
    let mut idents = BTreeSet::new();
    for ty in types {
        collect_idents(ty, &mut idents);
    }
    INPUT_NAMES.with(|names| {
        let names = names.borrow();
        idents.iter().any(|ident| names.contains_key(ident))
    })
}

/// The installed input-name map (for handing to worker threads).
pub fn snapshot() -> InputNames {
    INPUT_NAMES.with(|names| Arc::clone(&names.borrow()))
}

/// Install `names` on this thread.
pub fn install(names: InputNames) {
    INPUT_NAMES.with(|slot| *slot.borrow_mut() = names);
}

/// One documented type, as seen by [`compute_input_names`].
pub struct ViewSource<'a> {
    /// Component name.
    pub name: &'a str,
    /// Rust ident when it differs from `name` (`#[schema(name = ..)]`).
    pub rust_ident: Option<&'a str>,
    /// The parsed definition.
    pub item: &'a syn::Item,
}

/// Decide which types get an input view and name their components.
///
/// `taken` holds every existing component name (inputs never shadow one).
pub fn compute_input_names(sources: &[ViewSource<'_>], taken: &BTreeSet<String>) -> InputNames {
    // Ident (component name or Rust ident) → component name.
    let mut component_of: BTreeMap<&str, &str> = BTreeMap::new();
    for source in sources {
        component_of.insert(source.name, source.name);
        if let Some(ident) = source.rust_ident {
            component_of.entry(ident).or_insert(source.name);
        }
    }
    let mut differs: BTreeSet<&str> = sources
        .iter()
        .filter(|source| directly_differs(source.item))
        .map(|source| source.name)
        .collect();
    let referenced: Vec<(&str, BTreeSet<String>)> = sources
        .iter()
        .map(|source| (source.name, referenced_idents(source.item)))
        .collect();
    loop {
        let before = differs.len();
        for (name, idents) in &referenced {
            if idents
                .iter()
                .filter_map(|ident| component_of.get(ident.as_str()))
                .any(|component| differs.contains(component))
            {
                differs.insert(name);
            }
        }
        if differs.len() == before {
            break;
        }
    }

    let mut used = taken.clone();
    let mut names = BTreeMap::new();
    for name in differs {
        let input = free_input_name(name, &used);
        used.insert(input.clone());
        names.insert(name.to_string(), input);
    }
    for source in sources {
        if let Some(ident) = source.rust_ident
            && component_of
                .get(ident)
                .is_none_or(|owner| *owner == source.name)
            && let Some(input) = names.get(source.name).cloned()
        {
            names.entry(ident.to_string()).or_insert(input);
        }
    }
    Arc::new(names)
}

/// `{name}Input`, or `{name}Input2`, `{name}Input3`, ... when taken.
fn free_input_name(name: &str, used: &BTreeSet<String>) -> String {
    let base = format!("{name}Input");
    if !used.contains(&base) {
        return base;
    }
    // `used.len() + 2` candidates cannot all be taken.
    (2..=used.len() + 2)
        .map(|n| format!("{base}{n}"))
        .find(|candidate| !used.contains(candidate))
        .unwrap_or(base)
}

/// The type's own attributes make its request view differ.
fn directly_differs(item: &syn::Item) -> bool {
    let syn::Item::Struct(item) = item else {
        return false;
    };
    let syn::Fields::Named(fields) = &item.fields else {
        return false;
    };
    if fields.named.is_empty() {
        return false;
    }
    if extract_default(&item.attrs).is_some()
        || extract_rename_all_for(&item.attrs, SerdeSide::Serialize)
            != extract_rename_all_for(&item.attrs, SerdeSide::Deserialize)
    {
        return true;
    }
    fields.named.iter().any(|field| {
        let skip = extract_serde_skip_flags(&field.attrs);
        !skip.fully_skipped()
            && (skip.skip_deserializing
                || skip.skip_serializing
                || extract_default(&field.attrs).is_some()
                || extract_field_rename_for(&field.attrs, SerdeSide::Serialize)
                    != extract_field_rename_for(&field.attrs, SerdeSide::Deserialize))
    })
}

/// Every type ident named by the item's fields (generic arguments included).
fn referenced_idents(item: &syn::Item) -> BTreeSet<String> {
    let mut idents = BTreeSet::new();
    let mut add_fields = |fields: &syn::Fields| {
        for field in fields {
            collect_idents(&field.ty, &mut idents);
        }
    };
    match item {
        syn::Item::Struct(item) => add_fields(&item.fields),
        syn::Item::Enum(item) => item.variants.iter().for_each(|v| add_fields(&v.fields)),
        _ => {}
    }
    idents
}

fn collect_idents(ty: &syn::Type, idents: &mut BTreeSet<String>) {
    struct Visitor<'a>(&'a mut BTreeSet<String>);
    impl<'ast> syn::visit::Visit<'ast> for Visitor<'_> {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.0.insert(segment.ident.to_string());
            syn::visit::visit_path_segment(self, segment);
        }
    }
    syn::visit::Visit::visit_type(&mut Visitor(idents), ty);
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn names(sources: &[(&str, Option<&str>, &str)], taken: &[&str]) -> Vec<(String, String)> {
        let items: Vec<syn::Item> = sources
            .iter()
            .map(|(_, _, src)| syn::parse_str(src).unwrap())
            .collect();
        let views: Vec<ViewSource<'_>> = sources
            .iter()
            .zip(&items)
            .map(|((name, ident, _), item)| ViewSource {
                name,
                rust_ident: *ident,
                item,
            })
            .collect();
        let mut taken: BTreeSet<String> = taken.iter().map(ToString::to_string).collect();
        taken.extend(sources.iter().map(|(name, _, _)| name.to_string()));
        compute_input_names(&views, &taken)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[rstest]
    #[case::plain("struct A { x: i32, y: Option<u8> }", false)]
    #[case::field_default("struct A { #[serde(default)] x: i32 }", true)]
    #[case::container_default("#[serde(default)] struct A { x: i32 }", true)]
    #[case::skip_deserializing("struct A { #[serde(skip_deserializing)] id: i32 }", true)]
    #[case::skip_serializing("struct A { #[serde(skip_serializing)] pw: String }", true)]
    #[case::fully_skipped("struct A { #[serde(skip)] cache: u8, x: i32 }", false)]
    #[case::sided_rename(r#"struct A { #[serde(rename(deserialize = "in"))] x: i32 }"#, true)]
    #[case::same_rename(r#"struct A { #[serde(rename = "X")] x: i32 }"#, false)]
    #[case::sided_rename_all(
        r#"#[serde(rename_all(deserialize = "camelCase"))] struct A { a_b: i32 }"#,
        true
    )]
    #[case::skip_serializing_if(
        r#"struct A { #[serde(skip_serializing_if = "Vec::is_empty")] v: Vec<u8> }"#,
        false
    )]
    #[case::empty_container_default("#[serde(default)] struct A {}", false)]
    #[case::tuple("struct A(#[serde(default)] u8);", false)]
    #[case::enum_item("enum E { A { #[serde(default)] x: u8 } }", false)]
    fn direct_differences(#[case] src: &str, #[case] expected: bool) {
        assert_eq!(directly_differs(&syn::parse_str(src).unwrap()), expected);
    }

    #[test]
    fn differences_propagate_and_names_avoid_collisions() {
        let result = names(
            &[
                ("Line", None, "struct Line { #[serde(default)] qty: u32 }"),
                ("Order", None, "struct Order { lines: Vec<Line> }"),
                ("Wrapper", None, "enum Wrapper { One(Order), Two }"),
                ("Plain", None, "struct Plain { x: u8 }"),
                (
                    "UserDto",
                    Some("User"),
                    "struct User { #[serde(default)] x: u8 }",
                ),
            ],
            &["LineInput", "LineInput2"],
        );
        assert_eq!(
            result,
            [
                ("Line".into(), "LineInput3".into()),
                ("Order".into(), "OrderInput".into()),
                ("User".into(), "UserDtoInput".into()),
                ("UserDto".into(), "UserDtoInput".into()),
                ("Wrapper".into(), "WrapperInput".into()),
            ]
        );
    }

    #[test]
    fn ident_alias_never_shadows_a_real_component() {
        // `User` is a real component; the renamed `UserDto` (ident `User`)
        // must not claim the `User` key.
        let result = names(
            &[
                ("User", None, "struct User { x: u8 }"),
                (
                    "UserDto",
                    Some("User"),
                    "struct User { #[serde(default)] x: u8 }",
                ),
            ],
            &[],
        );
        assert_eq!(result, [("UserDto".into(), "UserDtoInput".into())]);
    }

    #[test]
    fn view_mode_and_installed_names() {
        install(Arc::new(BTreeMap::from([(
            "A".to_string(),
            "AInput".to_string(),
        )])));
        assert!(!in_request_view());
        assert_eq!(input_component("A"), None);
        assert!(has_input("A"));
        let inner = with_request_view(|| {
            assert!(in_request_view());
            (input_component("A"), input_component("B"))
        });
        assert_eq!(inner, (Some("AInput".to_string()), None));
        assert!(!in_request_view());
        assert_eq!(snapshot().len(), 1);
        let types: [syn::Type; 2] = [syn::parse_quote!(Vec<A>), syn::parse_quote!(u8)];
        assert!(mentions_input(&types));
        assert!(!mentions_input(&types[1..]));
        install(Arc::default());
        assert!(!has_input("A"));
    }

    #[test]
    fn non_type_items_reference_nothing() {
        let item: syn::Item = syn::parse_str("fn f() {}").unwrap();
        assert!(referenced_idents(&item).is_empty());
        assert!(!directly_differs(&item));
    }
}
