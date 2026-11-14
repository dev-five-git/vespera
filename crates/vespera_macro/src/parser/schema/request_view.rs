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
//! - the container is documented through a different type per direction
//!   (`#[serde(into = "A")]` and/or `from` / `try_from = "B"`, see
//!   `wire_override`),
//! - a field has a one-sided `serialize_with` / `deserialize_with` (without
//!   `#[schema(value_type)]`), or is an `Option` read through a custom
//!   deserializer (serde then requires it),
//! - a field has `#[serde(alias)]` names (accepted keys), or a non-`Option`
//!   `skip_serializing_if` field without `default` (optional on output,
//!   required on input),
//! - a tuple position is skipped / custom-converted on one side only,
//! - a field's type is a `HashSet` / `BTreeSet` (`uniqueItems` on output
//!   only) or a `std::time::Duration` (also read from `[secs, nanos]`),
//! - an enum variant is aliased, is the `#[serde(other)]` catch-all, or has
//!   a one-sided variant-level `serialize_with` / `deserialize_with`,
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

use quote::ToTokens;

use super::{
    enum_schema::derives_serde_repr,
    schema_attrs::extract_schema_constraints,
    serde_attrs::{
        SerdeSide, extract_aliases, extract_default, extract_field_codec, extract_field_rename_for,
        extract_rename_all_fields_for, extract_rename_all_for, extract_serde_skip_flags,
        extract_tag,
    },
    wire_override::container_override_type,
};
use crate::schema_macro::type_utils::is_option_type;

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

/// The serde direction of the view being built.
pub fn current_side() -> SerdeSide {
    if in_request_view() {
        SerdeSide::Deserialize
    } else {
        SerdeSide::Serialize
    }
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

/// The type (Rust ident or component name) whose input component is
/// `input`, if `input` is one.
pub fn component_of_input(input: &str) -> Option<String> {
    INPUT_NAMES.with(|names| {
        names
            .borrow()
            .iter()
            .find(|(_, name)| *name == input)
            .map(|(component, _)| component.clone())
    })
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

/// The container is documented through a different type per direction
/// (`#[serde(into = "A")]` without a matching `from` / `try_from`, …).
fn container_override_differs(attrs: &[syn::Attribute]) -> bool {
    let tokens =
        |side| container_override_type(attrs, side).map(|ty| ty.to_token_stream().to_string());
    tokens(SerdeSide::Serialize) != tokens(SerdeSide::Deserialize)
}

/// The field's documented schema or `required`-ness differs per direction
/// because of a custom serde function: a one-sided `serialize_with` /
/// `deserialize_with` without `value_type`, or an `Option` read through a
/// custom function (serde no longer defaults it to `None` when absent).
fn field_codec_differs(field: &syn::Field) -> bool {
    one_sided_codec(&field.attrs)
        || (extract_field_codec(&field.attrs).deserialize && is_option_type(&field.ty))
}

/// The type's own attributes make its request view differ.
fn directly_differs(item: &syn::Item) -> bool {
    match item {
        syn::Item::Struct(item) => container_override_differs(&item.attrs) || struct_differs(item),
        syn::Item::Enum(item) => container_override_differs(&item.attrs) || enum_differs(item),
        _ => false,
    }
}

/// A struct reads differently than it writes (container `default`, sided
/// `rename_all`, or a field that differs).
fn struct_differs(item: &syn::ItemStruct) -> bool {
    let fields = match &item.fields {
        syn::Fields::Named(fields) => fields,
        syn::Fields::Unnamed(fields) => return fields.unnamed.iter().any(positional_differs),
        syn::Fields::Unit => return false,
    };
    // serde writes a struct `tag` but never reads it (see `struct_schema`);
    // a serialize-side `value_type` / `into` hides it from the response view.
    if extract_tag(&item.attrs).is_some()
        && container_override_type(&item.attrs, SerdeSide::Serialize).is_none()
    {
        return true;
    }
    if fields.named.is_empty() {
        return false;
    }
    if extract_default(&item.attrs).is_some()
        || extract_rename_all_for(&item.attrs, SerdeSide::Serialize)
            != extract_rename_all_for(&item.attrs, SerdeSide::Deserialize)
    {
        return true;
    }
    fields.named.iter().any(field_differs)
}

/// A named field reads differently than it writes.
fn field_differs(field: &syn::Field) -> bool {
    let skip = extract_serde_skip_flags(&field.attrs);
    !skip.fully_skipped()
        && (skip.skip_deserializing
            || skip.skip_serializing
            || field_codec_differs(field)
            || field_type_differs(field)
            || extract_default(&field.attrs).is_some()
            || !extract_aliases(&field.attrs).is_empty()
            // Omitted from output when the predicate holds, but read as a
            // required key (serde has no default to fill it in).
            || (skip.skip_serializing_if && !is_option_type(&field.ty))
            || sided_differs(|side| extract_field_rename_for(&field.attrs, side)))
}

/// A tuple-struct / tuple-variant position reads differently than it
/// writes: skipped on one side only (the array length differs), a one-sided
/// custom serde function, or a type whose schema differs per side.
fn positional_differs(field: &syn::Field) -> bool {
    let skip = extract_serde_skip_flags(&field.attrs);
    !skip.fully_skipped()
        && (skip.skip_serializing != skip.skip_deserializing
            || one_sided_codec(&field.attrs)
            || field_type_differs(field))
}

/// A one-sided `serialize_with` / `deserialize_with` (field or variant)
/// without `#[schema(value_type)]`.
fn one_sided_codec(attrs: &[syn::Attribute]) -> bool {
    let codec = extract_field_codec(attrs);
    codec.serialize != codec.deserialize && extract_schema_constraints(attrs).value_type.is_none()
}

/// The field's type (or its `value_type`) has a different schema per side.
fn field_type_differs(field: &syn::Field) -> bool {
    type_view_differs(&field.ty)
        || extract_schema_constraints(&field.attrs)
            .value_type
            .is_some_and(|value_type| type_view_differs(&value_type.0))
}

/// Whether `ty` names a std type whose schema differs per side:
/// `HashSet` / `BTreeSet` (`uniqueItems` only on output — serde silently
/// drops duplicates when reading) and `std::time::Duration` (also read from
/// a `[secs, nanos]` array).
pub fn type_view_differs(ty: &syn::Type) -> bool {
    struct Visitor(bool);
    impl<'ast> syn::visit::Visit<'ast> for Visitor {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.0 |=
                path.segments.last().is_some_and(|segment| {
                    segment.ident == "HashSet" || segment.ident == "BTreeSet"
                }) || super::type_schema::is_std_duration_path(path);
            syn::visit::visit_path(self, path);
        }
    }
    let mut visitor = Visitor(false);
    syn::visit::Visit::visit_type(&mut visitor, ty);
    visitor.0
}

/// `extract` yields a different value per serde direction.
fn sided_differs<T: PartialEq>(extract: impl Fn(SerdeSide) -> T) -> bool {
    extract(SerdeSide::Serialize) != extract(SerdeSide::Deserialize)
}

/// An enum reads differently than it writes: one-sided `rename_all` /
/// `rename_all_fields` / `serde_repr`, a variant that is only skipped in one
/// direction, renamed per direction, aliased, a `#[serde(other)]` catch-all
/// (any unknown name reads as it), converted by a one-sided custom serde
/// function, or a variant field that differs like a struct field does.
fn enum_differs(item: &syn::ItemEnum) -> bool {
    let attrs = &item.attrs;
    if sided_differs(|side| extract_rename_all_for(attrs, side))
        || sided_differs(|side| extract_rename_all_fields_for(attrs, side))
        || sided_differs(|side| derives_serde_repr(attrs, side))
    {
        return true;
    }
    item.variants.iter().any(|variant| {
        let skip = extract_serde_skip_flags(&variant.attrs);
        !skip.skip
            && (skip.skip_serializing != skip.skip_deserializing
                || skip.other
                || !extract_aliases(&variant.attrs).is_empty()
                || one_sided_codec(&variant.attrs)
                || sided_differs(|side| extract_field_rename_for(&variant.attrs, side))
                || sided_differs(|side| extract_rename_all_for(&variant.attrs, side))
                || match &variant.fields {
                    syn::Fields::Named(fields) => fields.named.iter().any(field_differs),
                    syn::Fields::Unnamed(fields) => fields.unnamed.iter().any(positional_differs),
                    syn::Fields::Unit => false,
                })
    })
}

/// Every type ident named by the item's fields (generic arguments included),
/// their `#[schema(value_type = ..)]` types, and the container's
/// `value_type` / `into` / `from` / `try_from` types.
fn referenced_idents(item: &syn::Item) -> BTreeSet<String> {
    let mut idents = BTreeSet::new();
    let mut add_fields = |fields: &syn::Fields| {
        for field in fields {
            collect_idents(&field.ty, &mut idents);
            if let Some(value_type) = extract_schema_constraints(&field.attrs).value_type {
                collect_idents(&value_type.0, &mut idents);
            }
        }
    };
    let attrs = match item {
        syn::Item::Struct(item) => {
            add_fields(&item.fields);
            &item.attrs
        }
        syn::Item::Enum(item) => {
            item.variants.iter().for_each(|v| add_fields(&v.fields));
            &item.attrs
        }
        _ => return idents,
    };
    for side in [SerdeSide::Serialize, SerdeSide::Deserialize] {
        if let Some(ty) = container_override_type(attrs, side) {
            collect_idents(&ty, &mut idents);
        }
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
    // Optional on output, but serde reads it as a required key.
    #[case::skip_serializing_if(
        r#"struct A { #[serde(skip_serializing_if = "Vec::is_empty")] v: Vec<u8> }"#,
        true
    )]
    #[case::skip_serializing_if_option(
        r#"struct A { #[serde(skip_serializing_if = "Option::is_none")] v: Option<u8> }"#,
        false
    )]
    #[case::alias(r#"struct A { #[serde(alias = "b")] a: u8 }"#, true)]
    #[case::hash_set("struct A { tags: std::collections::HashSet<String> }", true)]
    #[case::btree_set_value_type(
        "struct A { #[schema(value_type = BTreeSet<u8>)] tags: Bits }",
        true
    )]
    #[case::std_duration("struct A { ttl: std::time::Duration }", true)]
    #[case::bare_duration("struct A { ttl: Duration }", false)]
    #[case::empty_container_default("#[serde(default)] struct A {}", false)]
    #[case::tuple("struct A(#[serde(default)] u8);", false)]
    #[case::tuple_one_sided_skip("struct A(u8, #[serde(skip_serializing)] u8);", true)]
    #[case::tuple_both_skips("struct A(u8, #[serde(skip)] u8);", false)]
    #[case::tuple_one_sided_codec(r#"struct A(#[serde(serialize_with = "f")] u8);"#, true)]
    #[case::tuple_set("struct A(HashSet<u8>);", true)]
    #[case::unit_struct("struct A;", false)]
    #[case::enum_variant_alias(r#"enum E { #[serde(alias = "a")] A, B }"#, true)]
    #[case::enum_other(r"enum E { A, #[serde(other)] Unknown }", true)]
    #[case::enum_variant_one_sided_codec(
        r#"enum E { #[serde(deserialize_with = "f")] A(u8) }"#,
        true
    )]
    #[case::enum_variant_with_both(r#"enum E { #[serde(with = "m")] A(u8) }"#, false)]
    #[case::enum_tuple_one_sided_skip("enum E { A(u8, #[serde(skip_deserializing)] u8) }", true)]
    #[case::enum_variant_field_default("enum E { A { #[serde(default)] x: u8 } }", true)]
    #[case::enum_plain("enum E { A, B(u8), C { x: u8 } }", false)]
    #[case::enum_sided_rename_all(
        r#"#[serde(rename_all(deserialize = "lowercase"))] enum E { A }"#,
        true
    )]
    #[case::enum_same_rename_all(r#"#[serde(rename_all = "lowercase")] enum E { A }"#, false)]
    #[case::enum_sided_rename_all_fields(
        r#"#[serde(rename_all_fields(serialize = "camelCase"))] enum E { A { x_y: u8 } }"#,
        true
    )]
    #[case::enum_variant_sided_rename(r#"enum E { #[serde(rename(deserialize = "a"))] A }"#, true)]
    #[case::enum_variant_skip_serializing("enum E { A, #[serde(skip_serializing)] B }", true)]
    #[case::enum_variant_skip_deserializing("enum E { A, #[serde(skip_deserializing)] B }", true)]
    #[case::enum_variant_skipped("enum E { A, #[serde(skip)] B }", false)]
    #[case::enum_serialize_repr_only("#[derive(Serialize_repr)] #[repr(u8)] enum E { A }", true)]
    #[case::enum_serde_repr_both(
        "#[derive(Serialize_repr, Deserialize_repr)] #[repr(u8)] enum E { A }",
        false
    )]
    #[case::into_only(r#"#[serde(into = "W")] struct A { x: u8 }"#, true)]
    #[case::from_only(r#"#[serde(from = "W")] struct A(u8);"#, true)]
    #[case::into_and_from_same(r#"#[serde(into = "W", from = "W")] struct A { x: u8 }"#, false)]
    #[case::container_value_type(
        r#"#[schema(value_type = W)] #[serde(into = "X")] struct A { x: u8 }"#,
        false
    )]
    #[case::enum_into(r#"#[serde(into = "String")] enum E { A }"#, true)]
    #[case::with_both_sides(r#"struct A { #[serde(with = "m")] x: u8 }"#, false)]
    #[case::serialize_with(r#"struct A { #[serde(serialize_with = "f")] x: u8 }"#, true)]
    #[case::deserialize_with_value_type(
        r#"struct A { #[serde(deserialize_with = "f")] #[schema(value_type = u8)] x: u8 }"#,
        false
    )]
    #[case::option_with(r#"struct A { #[serde(with = "m")] x: Option<u8> }"#, true)]
    #[case::struct_tag(r#"#[serde(tag = "type")] struct A { x: u8 }"#, true)]
    #[case::struct_tag_no_fields(r#"#[serde(tag = "type")] struct A {}"#, true)]
    #[case::struct_tag_value_type(
        r#"#[schema(value_type = W)] #[serde(tag = "type")] struct A { x: u8 }"#,
        false
    )]
    #[case::struct_tag_into(r#"#[serde(tag = "type", into = "W")] struct A { x: u8 }"#, true)]
    #[case::form_data_over_serde_rename(
        r#"struct A { #[serde(rename = "a")] #[form_data(field_name = "b")] x: u8 }"#,
        true
    )]
    #[case::form_data_only(r#"struct A { #[form_data(field_name = "b")] x: u8 }"#, false)]
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
        assert_eq!(component_of_input("AInput").as_deref(), Some("A"));
        assert_eq!(component_of_input("A"), None);
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

    #[rstest]
    #[case::field_value_type(
        "struct A { #[schema(value_type = Vec<W>)] x: u8 }",
        &["Vec", "W", "u8"]
    )]
    #[case::struct_conversions(
        r#"#[serde(into = "Out", try_from = "In")] struct A { x: u8 }"#,
        &["In", "Out", "u8"]
    )]
    #[case::enum_conversion(
        r#"#[serde(from = "Raw")] enum E { V(Payload) }"#,
        &["Payload", "Raw"]
    )]
    fn referenced_idents_cover_overrides(#[case] src: &str, #[case] expected: &[&str]) {
        let item: syn::Item = syn::parse_str(src).unwrap();
        let mut expected: Vec<&str> = expected.to_vec();
        expected.sort_unstable();
        let found = referenced_idents(&item);
        assert_eq!(
            found.iter().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn container_conversion_propagates_input_views() {
        // `Order` deserializes from `RawOrder`, whose request view differs;
        // `Order` itself only names `u8` fields.
        let result = names(
            &[
                (
                    "RawOrder",
                    None,
                    "struct RawOrder { #[serde(default)] qty: u32 }",
                ),
                (
                    "Order",
                    None,
                    r"#[schema(value_type = RawOrder)] struct Order { qty: u8 }",
                ),
            ],
            &[],
        );
        assert_eq!(
            result,
            [
                ("Order".into(), "OrderInput".into()),
                ("RawOrder".into(), "RawOrderInput".into()),
            ]
        );
    }

    #[test]
    fn current_side_follows_the_view() {
        assert_eq!(current_side(), SerdeSide::Serialize);
        assert_eq!(with_request_view(current_side), SerdeSide::Deserialize);
    }

    #[test]
    fn non_type_items_reference_nothing() {
        let item: syn::Item = syn::parse_str("fn f() {}").unwrap();
        assert!(referenced_idents(&item).is_empty());
        assert!(!directly_differs(&item));
    }
}
