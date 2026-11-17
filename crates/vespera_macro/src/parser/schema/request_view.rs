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
//! - a field has a `with` / `serialize_with` (without
//!   `#[schema(value_type)]`: the request view documents the Rust type),
//!   or is an `Option` read through a custom
//!   deserializer (serde then requires it),
//! - a field has `#[serde(alias)]` names (accepted keys), a non-`Option`
//!   `skip_serializing_if` field without `default` (optional on output,
//!   required on input), or an `Option` field without `skip_serializing_if`
//!   (always written, as `null` when `None`, so required on output; optional
//!   on input) or with `skip_serializing_if = "Option::is_none"` (never
//!   `null` on output; nullable on input),
//! - a tuple position is skipped / custom-converted on one side only, or a
//!   tuple of two or more fields has a `#[serde(default)]` position (or
//!   container `default`): trailing items may be omitted, or a
//!   `skip_serializing_if` position (omitted, and the rest shifted, on output
//!   only),
//! - a field's type is a `HashSet` / `BTreeSet` (`uniqueItems` on output
//!   only), a `std::time::Duration` (also read from `[secs, nanos]`) or a
//!   `Decimal` (also read from a JSON number),
//! - an externally tagged enum mixes unit and data variants (a unit variant
//!   is also read from the map form `{"Name": null}`),
//! - a `#[serde(flatten)]` field is an `Option` (its keys accept anything,
//!   failing values make it `None`), follows a flattened map (which also
//!   reads its keys), is a type parameter (it may be either), or leaves its
//!   keys in serde's buffer ([`flat_read`]: a struct with a flatten field
//!   of its own, an internally tagged / untagged enum) under
//!   `deny_unknown_fields` (which then rejects them) or before a flattened
//!   map (which then reads them) — seen through its `value_type` and
//!   `Box` / `Arc` / `Rc` / `Cow` / `&`, like the schema builder sees it,
//! - an enum variant is aliased, is the `#[serde(other)]` catch-all, or has
//!   a one-sided variant-level `serialize_with` / `deserialize_with`, or is
//!   an adjacently tagged `Option` newtype (its content key may be omitted),
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
    scoped_ident::lookup_key,
    serde_attrs::{
        SerdeEnumRepr, SerdeSide, extract_aliases, extract_default, extract_deny_unknown_fields,
        extract_enum_repr, extract_field_codec, extract_field_rename_for, extract_flatten,
        extract_rename_all_fields_for, extract_rename_all_for, extract_serde_skip_flags,
        extract_tag, extract_transparent, extract_untagged, skips_none_on_output,
    },
    struct_schema::{flattened_member, peel},
    wire_override::{codec_hides_shape, container_override_type},
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
            .find(|(component, name)| *name == input && !component.contains("::"))
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
        idents.iter().any(|ident| {
            names.contains_key(ident)
                || ident
                    .rsplit_once("::")
                    .is_some_and(|(_, bare)| names.contains_key(bare))
        })
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
    /// `module::Ident` key of a renamed type whose declaring module is known
    /// (see `parser::schema::definition_key`).
    pub scoped_keys: Vec<&'a str>,
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
        for ident in source
            .rust_ident
            .into_iter()
            .chain(source.scoped_keys.iter().copied())
        {
            component_of.entry(ident).or_insert(source.name);
        }
    }
    // `b::Foo` resolves through its scoped key when one is registered, else
    // through the bare ident (as `definition_key` does).
    let owner = |ident: &str| {
        component_of.get(ident).or_else(|| {
            ident
                .rsplit_once("::")
                .and_then(|(_, bare)| component_of.get(bare))
        })
    };
    let items: Vec<&syn::Item> = sources.iter().map(|source| source.item).collect();
    let flat = FlatKinds::of(&items);
    let mut differs: BTreeSet<&str> = sources
        .iter()
        .filter(|source| directly_differs(source.item, &flat))
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
                .filter_map(|ident| owner(ident))
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
        for ident in source
            .rust_ident
            .into_iter()
            .chain(source.scoped_keys.iter().copied())
        {
            if component_of
                .get(ident)
                .is_none_or(|owner| *owner == source.name)
                && let Some(input) = names.get(source.name).cloned()
            {
                names.entry(ident.to_string()).or_insert(input);
            }
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
/// because of a custom serde function: a serialize-side one (`with` /
/// `serialize_with`) without `value_type` hides the response shape while
/// the request view documents the Rust type (see `wire_override`), and an
/// `Option` read through a custom function is required (serde no longer
/// defaults it to `None` when absent). A `deserialize_with` alone documents
/// the same type on both sides.
fn field_codec_differs(field: &syn::Field) -> bool {
    codec_differs(&field.attrs)
        || (extract_field_codec(&field.attrs).deserialize && is_option_type(&field.ty))
}

/// A field's custom serde function documents a different schema per side
/// (see [`field_codec_differs`]).
fn codec_differs(attrs: &[syn::Attribute]) -> bool {
    codec_hides_shape(attrs, SerdeSide::Serialize)
        && extract_schema_constraints(attrs).value_type.is_none()
}

/// The type's own attributes make its request view differ. `flat` classifies
/// the types a flattened member may name (see [`FlatKinds`]).
fn directly_differs(item: &syn::Item, flat: &FlatKinds) -> bool {
    match item {
        syn::Item::Struct(item) => {
            container_override_differs(&item.attrs) || struct_differs(item, flat)
        }
        syn::Item::Enum(item) => {
            container_override_differs(&item.attrs) || enum_differs(item, flat)
        }
        _ => false,
    }
}

/// How serde reads a type `#[serde(flatten)]`ed into another struct, from
/// the outer struct's buffer of the keys it does not know (serde 1.0.229,
/// `FlatMapDeserializer`).
pub(in crate::parser::schema) enum FlatRead<'a> {
    /// It TAKES the keys it reads (`deserialize_struct` → `FlatStructAccess`
    /// for a struct without flatten fields or an adjacently tagged enum,
    /// `deserialize_enum` for an externally tagged one), or reads nothing:
    /// they are gone for the outer `deny_unknown_fields` check and for every
    /// map flattened after it.
    Takes,
    /// It reads through `FlatMapAccess`, which only BORROWS the buffered
    /// entries: a struct with a (deserialized) flatten field of its own
    /// (`deserialize_map`), an internally tagged / untagged enum or one with
    /// an untagged variant (`deserialize_any`). Its keys stay in the buffer,
    /// so the outer `deny_unknown_fields` rejects them and every flattened
    /// map reads them. `known_keys`: a struct (its keys can be listed).
    Leaves { known_keys: bool },
    /// A newtype (`deserialize_newtype_struct` hands it the buffer as is)
    /// or `#[serde(transparent)]` wrapper: read like this type.
    Through(&'a syn::Type),
}

/// How serde reads `item` when it is flattened (see [`FlatRead`]).
pub(in crate::parser::schema) fn flat_read(item: &syn::Item) -> FlatRead<'_> {
    let read = |field: &&syn::Field| {
        let skip = extract_serde_skip_flags(&field.attrs);
        !skip.skip && !skip.skip_deserializing
    };
    match item {
        syn::Item::Struct(item) => {
            let transparent = extract_transparent(&item.attrs);
            match &item.fields {
                syn::Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                    FlatRead::Through(&fields.unnamed[0].ty)
                }
                fields if transparent => fields
                    .iter()
                    .find(read)
                    .map_or(FlatRead::Takes, |field| FlatRead::Through(&field.ty)),
                syn::Fields::Named(fields)
                    if fields
                        .named
                        .iter()
                        .filter(read)
                        .any(|field| extract_flatten(&field.attrs)) =>
                {
                    FlatRead::Leaves { known_keys: true }
                }
                _ => FlatRead::Takes,
            }
        }
        syn::Item::Enum(item)
            if !matches!(
                extract_enum_repr(&item.attrs),
                SerdeEnumRepr::ExternallyTagged | SerdeEnumRepr::AdjacentlyTagged { .. }
            ) || item
                .variants
                .iter()
                .any(|variant| extract_untagged(&variant.attrs)) =>
        {
            FlatRead::Leaves { known_keys: false }
        }
        _ => FlatRead::Takes,
    }
}

/// What a flattened member does with the keys serde buffered for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlatKind {
    /// Reads EVERY buffered key: a map, or a struct flattening one.
    Map,
    /// Reads only its own keys but leaves them in the buffer (see
    /// [`FlatRead::Leaves`]); `known_keys` as there.
    Leaves { known_keys: bool },
}

/// The [`FlatKind`] of every type among the documented items whose
/// flattened form reads every buffered key or leaves its keys behind —
/// directly, through a flattened member (a struct flattening a map reads
/// every key), or through a newtype / transparent wrapper (transitively).
struct FlatKinds(BTreeMap<String, FlatKind>);

impl FlatKinds {
    fn of(items: &[&syn::Item]) -> Self {
        let mut kinds = Self(BTreeMap::new());
        loop {
            let mut changed = false;
            for item in items {
                let ident = match item {
                    syn::Item::Struct(item) => &item.ident,
                    syn::Item::Enum(item) => &item.ident,
                    _ => continue,
                };
                if let Some(kind) = kinds.item_kind(item)
                    && kinds.0.insert(ident.to_string(), kind) != Some(kind)
                {
                    changed = true;
                }
            }
            if !changed {
                return kinds;
            }
        }
    }

    /// An item's kind: a struct is a map when any flattened member reads
    /// every key.
    fn item_kind(&self, item: &syn::Item) -> Option<FlatKind> {
        if let syn::Item::Struct(item) = item
            && let syn::Fields::Named(fields) = &item.fields
            && fields
                .named
                .iter()
                .filter(|field| extract_flatten(&field.attrs))
                .any(|field| self.field_kind(field) == Some(FlatKind::Map))
        {
            return Some(FlatKind::Map);
        }
        match flat_read(item) {
            FlatRead::Takes => None,
            FlatRead::Leaves { known_keys } => Some(FlatKind::Leaves { known_keys }),
            FlatRead::Through(ty) => self.type_kind(ty),
        }
    }

    /// The kind of a flattened `field` (its `value_type`, seen through
    /// `Box` / `Arc` / ... and an `Option`, exactly as
    /// `struct_schema::flatten` sees it).
    fn field_kind(&self, field: &syn::Field) -> Option<FlatKind> {
        let constraints = extract_schema_constraints(&field.attrs);
        self.type_kind(flattened_member(field, &constraints).0)
    }

    fn type_kind(&self, ty: &syn::Type) -> Option<FlatKind> {
        let ty = peel(ty);
        if is_map_type(ty) {
            return Some(FlatKind::Map);
        }
        let syn::Type::Path(path) = ty else {
            return None;
        };
        let ident = path.path.segments.last()?.ident.to_string();
        self.0.get(&ident).copied()
    }
}

/// A struct reads differently than it writes (container `default`, sided
/// `rename_all`, or a field that differs).
fn struct_differs(item: &syn::ItemStruct, flat: &FlatKinds) -> bool {
    let fields = match &item.fields {
        syn::Fields::Named(fields) => fields,
        syn::Fields::Unnamed(fields) => {
            return tuple_differs(fields, extract_default(&item.attrs).is_some());
        }
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
        || flatten_differs(
            &fields.named,
            &item.generics,
            extract_deny_unknown_fields(&item.attrs),
            flat,
        )
}

/// The merged `#[serde(flatten)]` members read differently than they write
/// (see `struct_schema::flatten`): a flattened `Option` accepts any value for
/// its keys (it becomes `None`), a flattened map (or a struct flattening
/// one) declared before another flattened member also reads that member's
/// keys, a member that leaves its keys in the buffer ([`FlatRead::Leaves`])
/// has them read by a map declared after it too, and under
/// `deny_unknown_fields` a flattened map is written but never read (the
/// request view stays closed) and a member leaving its keys has them
/// rejected. A flattened type parameter may be any of these
/// (`W<Option<Page>>`), so it differs too. Members are seen through
/// `value_type` / `Box` / `Arc` / ... like the schema builder sees them
/// ([`flattened_member`]).
fn flatten_differs(
    fields: &syn::punctuated::Punctuated<syn::Field, syn::Token![,]>,
    generics: &syn::Generics,
    deny_unknown_fields: bool,
    flat: &FlatKinds,
) -> bool {
    let mut kinds = Vec::new();
    for field in fields.iter().filter(|field| extract_flatten(&field.attrs)) {
        let constraints = extract_schema_constraints(&field.attrs);
        let (ty, optional) = flattened_member(field, &constraints);
        if optional || is_type_param(peel(ty), generics) {
            return true;
        }
        kinds.push(flat.field_kind(field));
    }
    let maps = kinds
        .iter()
        .filter(|kind| **kind == Some(FlatKind::Map))
        .count();
    let mut maps_seen = 0;
    kinds.iter().any(|kind| {
        let follows_map = maps_seen > 0;
        if *kind == Some(FlatKind::Map) {
            maps_seen += 1;
        }
        follows_map
            || (kind.is_some() && deny_unknown_fields)
            || (*kind == Some(FlatKind::Leaves { known_keys: true }) && maps > maps_seen)
    })
}

/// `ty` is one of `generics`' type parameters (`T`).
fn is_type_param(ty: &syn::Type, generics: &syn::Generics) -> bool {
    matches!(ty, syn::Type::Path(path) if path.qself.is_none()
    && path.path.get_ident().is_some_and(|ident| {
        generics.type_params().any(|param| param.ident == *ident)
    }))
}

/// A map type serde reads through `deserialize_map` (by its last segment).
fn is_map_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
        ["HashMap", "BTreeMap", "IndexMap", "Map"].iter().any(|name| segment.ident == name)
    }))
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
            || crate::multipart_impl::has_form_data_default(&field.attrs)
            || !extract_aliases(&field.attrs).is_empty()
            // `required` differs: a `skip_serializing_if` field may be
            // omitted from output but is a required key on input (serde has
            // no default to fill it in), and an `Option` is always written
            // (as `null`) but may be omitted on input. An `Option` with a
            // predicate is optional on both sides — and with
            // `Option::is_none` never `null` on output, while input accepts
            // `null`. A flattened `Option` has no key of its own (see
            // [`flatten_differs`]).
            || (skip.skip_serializing_if && !is_option_type(&field.ty))
            || (is_option_type(&field.ty)
                && (!skip.skip_serializing_if || skips_none_on_output(&field.attrs))
                && !extract_flatten(&field.attrs))
            || sided_differs(|side| extract_field_rename_for(&field.attrs, side)))
}

/// A tuple-struct / tuple-variant position reads differently than it
/// writes: skipped on one side only (the array length differs), a
/// serialize-side custom serde function, or a type whose schema differs
/// per side.
fn positional_differs(field: &syn::Field) -> bool {
    let skip = extract_serde_skip_flags(&field.attrs);
    !skip.fully_skipped()
        && (skip.skip_serializing != skip.skip_deserializing
            || codec_differs(&field.attrs)
            || field_type_differs(field))
}

/// A tuple struct / tuple variant reads differently than it writes: a
/// position differs, or — for a real tuple (two or more declared fields;
/// serde reads one field as a newtype, never as an array) — a position (or
/// the tuple struct, `container_default`) has a `#[serde(default)]`, which
/// lets the trailing array items be omitted, or a `skip_serializing_if`
/// (the response omits that item and shifts the rest left; serde still
/// reads every position, see `struct_schema::tuple_fields_schema`).
fn tuple_differs(fields: &syn::FieldsUnnamed, container_default: bool) -> bool {
    fields.unnamed.iter().any(positional_differs)
        || (fields.unnamed.len() > 1
            && (container_default
                || fields.unnamed.iter().any(|field| {
                    let skip = extract_serde_skip_flags(&field.attrs);
                    !skip.skip
                        && !skip.skip_deserializing
                        && (extract_default(&field.attrs).is_some()
                            || (skip.skip_serializing_if && !skip.skip_serializing))
                })))
}

/// A one-sided variant-level `serialize_with` / `deserialize_with`
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
/// drops duplicates when reading), `std::time::Duration` (also read from
/// a `[secs, nanos]` array) and `Decimal` (also read from a JSON number).
pub fn type_view_differs(ty: &syn::Type) -> bool {
    struct Visitor(bool);
    impl<'ast> syn::visit::Visit<'ast> for Visitor {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.0 |= path.segments.last().is_some_and(|segment| {
                ["HashSet", "BTreeSet", "Decimal"]
                    .iter()
                    .any(|name| segment.ident == name)
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
/// function, a variant field (or flattened member) that differs like a
/// struct's does, or an adjacently tagged `Option` newtype variant (its
/// content key may be omitted on input, see
/// `representations::parse_adjacently_tagged_enum`).
fn enum_differs(item: &syn::ItemEnum, flat: &FlatKinds) -> bool {
    let attrs = &item.attrs;
    let deny_unknown_fields = extract_deny_unknown_fields(attrs);
    let adjacently_tagged = matches!(
        extract_enum_repr(attrs),
        SerdeEnumRepr::AdjacentlyTagged { .. }
    );
    if sided_differs(|side| extract_rename_all_for(attrs, side))
        || sided_differs(|side| extract_rename_all_fields_for(attrs, side))
        || sided_differs(|side| derives_serde_repr(attrs, side))
    {
        return true;
    }
    if matches!(extract_enum_repr(attrs), SerdeEnumRepr::ExternallyTagged)
        && mixes_unit_and_data(item)
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
                    syn::Fields::Named(fields) => {
                        fields.named.iter().any(field_differs)
                            || flatten_differs(
                                &fields.named,
                                &item.generics,
                                deny_unknown_fields,
                                flat,
                            )
                    }
                    syn::Fields::Unnamed(fields) => {
                        tuple_differs(fields, false)
                            || (adjacently_tagged
                                && fields.unnamed.len() == 1
                                && is_option_type(&fields.unnamed[0].ty)
                                && !extract_field_codec(&variant.attrs).deserialize)
                    }
                    // A content key a unit variant with a `deserialize_with` may omit.
                    syn::Fields::Unit => {
                        adjacently_tagged && extract_field_codec(&variant.attrs).deserialize
                    }
                })
    })
}

/// The externally tagged variants serde reads (not `#[serde(untagged)]`,
/// not skipped on input) mix unit and data variants: the request view then
/// also accepts each unit variant in the map form `{"Name": null}`
/// (`representations::parse_externally_tagged_enum`), which the response
/// view never writes. An all-unit enum stays a plain string enum in both
/// views (see `unit::parse_unit_enum_to_schema`).
fn mixes_unit_and_data(item: &syn::ItemEnum) -> bool {
    let (mut unit, mut data) = (false, false);
    for variant in &item.variants {
        let skip = extract_serde_skip_flags(&variant.attrs);
        if skip.skip || skip.skip_deserializing || extract_untagged(&variant.attrs) {
            continue;
        }
        // A unit variant with a `deserialize_with` is read as a newtype.
        if matches!(variant.fields, syn::Fields::Unit)
            && !extract_field_codec(&variant.attrs).deserialize
        {
            unit = true;
        } else {
            data = true;
        }
    }
    unit && data
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
        /// Every segment ident; a module-qualified type (`b::Foo`) as its
        /// registry key instead of the bare ident, so it resolves to the
        /// declaration its path names.
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(last) = path.segments.last() {
                self.0
                    .insert(lookup_key(path).unwrap_or_else(|| last.ident.to_string()));
            }
            for segment in path.segments.iter().rev().skip(1) {
                self.0.insert(segment.ident.to_string());
            }
            syn::visit::visit_path(self, path);
        }
    }
    syn::visit::Visit::visit_type(&mut Visitor(idents), ty);
}

#[cfg(test)]
mod tests;
