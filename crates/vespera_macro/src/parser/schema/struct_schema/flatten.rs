//! `#[serde(flatten)]` members merged into the enclosing object.
//!
//! serde's derive (serde 1.0.229, `serde_derive::de::struct_`) buffers every
//! key the outer struct does not know itself, then deserializes each
//! flattened member from that buffer:
//!
//! - a flattened STRUCT without flatten fields of its own takes only the
//!   keys it knows (`FlatStructAccess`), so its own `deny_unknown_fields`
//!   never fires — its fields are simply more keys of the outer object;
//! - a flattened struct WITH a flatten field of its own (read through
//!   `deserialize_map`) and a flattened internally tagged / untagged enum
//!   (`deserialize_any`) read through `FlatMapAccess` and take nothing
//!   (see `request_view::FlatRead`): their keys stay in the buffer, so the
//!   outer `deny_unknown_fields` rejects them (in the request view their
//!   properties are `{not: {}}`, and such an opaque member does not open
//!   the object) and every flattened map reads them, also one declared
//!   after them;
//! - a flattened MAP reads every key still in the buffer
//!   (`FlatMapAccess`) WITHOUT taking it: it types the outer object's
//!   unlisted keys AND every key of a flattened member declared after it
//!   (that member takes its keys only later), so in the request view those
//!   members' properties must satisfy the map's value schema too;
//! - a flattened `Option<T>` is `None` whenever `T` fails, so it never makes
//!   a key required, and in the request view its keys reject nothing;
//! - a flattened unit struct reads nothing;
//! - a flattened externally tagged enum takes the FIRST buffered key naming
//!   a variant (`FlatMapDeserializer::deserialize_enum`) and leaves any
//!   other variant key to the outer struct, so its request-view branches are
//!   an `anyOf` (several may match), never a `oneOf`;
//! - the OUTER `deny_unknown_fields` rejects any key no member consumed
//!   (a map borrows, never consumes, so with it the map stays empty);
//! - a flattened newtype (`deserialize_newtype_struct` passes the buffer
//!   on) or `#[serde(transparent)]` wrapper reads like the type it wraps.
//!
//! JSON Schema `additionalProperties` only sees the `properties` of its own
//! schema object, so composing members with `allOf` gets these semantics
//! wrong (an outer `{"id": 1}` would be checked against a flattened map's
//! value schema, and a `deny_unknown_fields` member would reject every outer
//! key). Members whose keys are known are therefore MERGED into one object,
//! except a plain struct member with no flattened map and no closed sibling,
//! which stays a `$ref` `allOf` member so its component is reused (see
//! `assemble`). Only members whose keys cannot be known (a flattened enum, an
//! unresolvable type) stay `allOf` members — and they keep the merged object
//! open. Such a member's resolved schema is inlined with every object inside
//! it reopened (see `embedded`: closed branches, map variants, `$ref`
//! branches): it never sees the outer keys, so it must not reject them. A
//! newtype / transparent wrapper (`allOf: [$ref]`) and a recursive generic
//! instance (its materialised component) are merged like the struct or
//! flattened like the enum they name.

use std::{
    borrow::Borrow,
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{AdditionalProperties, Schema, SchemaRef, SchemaType};

use super::{
    super::{
        embedded::{open_embedded, unwrap_wrappers, without_unlisted_bounds},
        enum_schema::{parse_enum_to_schema, without_other_map, without_single_key_bounds},
        request_view::{self, FlatRead},
        schema_attrs::{SchemaConstraints, extract_schema_constraints},
        serde_attrs::SerdeSide,
        type_schema::{generic_instances, parse_type_to_schema_ref},
        wire_override::codec_applies,
    },
    parse_struct_to_schema,
};
use crate::schema_macro::type_utils::option_inner;

thread_local! {
    /// Rust idents whose definitions are being resolved (cycle guard: a
    /// struct flattening itself through `Option<Box<Self>>`).
    static RESOLVING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Keys and constraints of an object, ready to be merged.
#[derive(Default)]
pub(super) struct ObjectParts {
    pub properties: BTreeMap<String, SchemaRef>,
    pub required: Vec<String>,
    /// Value schema of the unlisted keys (a flattened map).
    pub additional: Option<SchemaRef>,
    /// `allOf` members constraining listed keys only (alias groups).
    pub constraints: Vec<SchemaRef>,
    /// Members whose keys are unknown (flattened enums, unresolved types).
    pub opaque: Vec<SchemaRef>,
    /// The source object was closed (`additionalProperties: false`), so its
    /// `$ref` cannot be composed with other keys.
    pub closed: bool,
}

/// One `#[serde(flatten)]` field. `leaves`: serde leaves its keys in the
/// outer buffer ([`leaves_keys`]).
pub(super) enum FlatPart {
    /// A member whose keys are known; `reference` is its `$ref`, if any.
    Known {
        reference: Option<SchemaRef>,
        parts: ObjectParts,
        optional: bool,
        leaves: bool,
    },
    /// A member whose keys cannot be known.
    Opaque { schema: SchemaRef, leaves: bool },
}

/// Classify a `#[serde(flatten)]` field on `side`; an `omittable` member
/// (`skip_serializing(_if)` in the response view) is optional like a
/// flattened `Option`.
pub(super) fn flatten_part(
    field: &syn::Field,
    side: SerdeSide,
    omittable: bool,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> FlatPart {
    let constraints = extract_schema_constraints(&field.attrs);
    // serde sees through `Box` / `Arc` / ... to a flattened `Option` too.
    let (ty, optional) = flattened_member(field, &constraints);
    let optional = optional || omittable;
    // `#[schema(value_type = T)]` documents a custom codec's wire shape,
    // exactly as on a field that is not flattened.
    let custom = constraints.is_any()
        || (constraints.value_type.is_none() && codec_applies(&field.attrs, side));
    let leaves = !custom && leaves_keys(ty, struct_definitions, WRAPPER_DEPTH);
    let schema = if custom {
        SchemaRef::Inline(Box::default())
    } else {
        parse_type_to_schema_ref(ty, known_schemas, struct_definitions)
    };
    let (resolved, reference) = match &schema {
        SchemaRef::Inline(inline) => (Some((**inline).clone()), None),
        // A recursive generic instance (`Tree<u8>`) has no definition of its
        // own, but its materialised component.
        SchemaRef::Ref(reference) => (
            resolve_type(ty, known_schemas, struct_definitions)
                .or_else(|| resolve_reference(reference, known_schemas, struct_definitions)),
            Some(schema.clone()),
        ),
    };
    if let Some(parts) = resolved
        .as_ref()
        .and_then(|schema| decompose(schema, known_schemas, struct_definitions))
    {
        return FlatPart::Known {
            // A unit struct (`null`) has no keys: its `$ref` would make an
            // `allOf` member nothing satisfies next to the object.
            reference: reference.filter(|_| {
                resolved
                    .as_ref()
                    .is_some_and(|schema| schema.schema_type != Some(SchemaType::Null))
            }),
            parts,
            optional,
            leaves,
        };
    }
    // A newtype / transparent wrapper around an enum is flattened like that
    // enum. An externally tagged enum's variant key sits among the other
    // keys and its unit variants are keys when flattened (`keyed`).
    let schema = resolved
        .map(|resolved| unwrap_wrappers(resolved, known_schemas, struct_definitions))
        .and_then(|resolved| {
            // Each step rewrites the result of the previous one, if any.
            let step = |done: Option<Schema>, rewrite: fn(&Schema) -> Option<Schema>| {
                rewrite(done.as_ref().unwrap_or(&resolved)).or(done)
            };
            let open = step(None, without_single_key_bounds);
            let flattened = step(open, flattened_unit_variants);
            // serde never hands an unknown key to a flattened `other`.
            let keyed = flattened.is_some();
            let mut flattened = step(flattened, without_other_map);
            // serde reads the FIRST buffered key naming a variant and leaves
            // any other one to the outer struct, so on input several keyed
            // branches may match at once (`{"A": 1, "B": 2}`): `anyOf`.
            if keyed
                && side == SerdeSide::Deserialize
                && let Some(schema) = flattened.as_mut()
                && schema.any_of.is_none()
            {
                schema.any_of = schema.one_of.take();
            }
            // A member that cannot be merged only sees the keys the outer
            // struct does not know: no object inside it (a closed struct, a
            // map, a nested enum's single-key branch) may reject them.
            open_embedded(
                flattened.as_ref().unwrap_or(&resolved),
                &without_unlisted_bounds,
                known_schemas,
                struct_definitions,
            )
            .or(flattened)
        })
        .map_or(schema, |flattened| SchemaRef::Inline(Box::new(flattened)));
    // serde turns any failure of an optional member into `None`.
    let schema = if optional { lenient(schema) } else { schema };
    FlatPart::Opaque { schema, leaves }
}

/// How many newtype / transparent wrappers [`leaves_keys`] looks through
/// (a bound for `struct S(Box<S>)`).
const WRAPPER_DEPTH: usize = 8;

/// Whether serde leaves the keys of a flattened `ty` in the outer buffer
/// (`request_view::FlatRead::Leaves`, seen through `depth` newtype /
/// transparent wrappers): its definition must be known.
fn leaves_keys(
    ty: &syn::Type,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
    depth: usize,
) -> bool {
    let syn::Type::Path(type_path) = peel(ty) else {
        return false;
    };
    super::super::definition_key(&type_path.path, |key| struct_definitions.contains_key(key))
        .and_then(|key| struct_definitions.get(key.as_str()))
        .and_then(|definition| syn::parse_str::<syn::Item>(definition.as_ref()).ok())
        .is_some_and(|item| match request_view::flat_read(&item) {
            FlatRead::Takes => false,
            FlatRead::Leaves { .. } => true,
            FlatRead::Through(inner) => {
                depth > 0 && leaves_keys(inner, struct_definitions, depth - 1)
            }
        })
}

/// The flattened form of an externally tagged enum `schema` with unit
/// variants: serde writes (and reads) a flattened unit variant as the key
/// `{"A": null}`, never as the string `"A"`, so every `{type: string, enum:
/// [..]}` branch (or the whole schema of a unit-only enum) becomes one
/// `{A: null}` object per name (a name that already has such a branch — the
/// request view's map form, see `parse_externally_tagged_enum` — is not
/// listed twice). `None` when there is no such branch.
pub(in crate::parser::schema) fn flattened_unit_variants(schema: &Schema) -> Option<Schema> {
    let existing: Vec<&str> = schema
        .one_of
        .iter()
        .flatten()
        .filter_map(null_key_branch)
        .collect();
    let unit_keys = |schema: &Schema| {
        (schema.schema_type == Some(SchemaType::String) && schema.not.is_none())
            .then_some(schema.r#enum.as_ref())
            .flatten()
            .map(|keys| {
                keys.iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|key| !existing.contains(key))
                    .map(|key| {
                        SchemaRef::Inline(Box::new(Schema {
                            description: schema.description.clone(),
                            properties: Some(BTreeMap::from([(
                                key.to_string(),
                                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Null))),
                            )])),
                            required: Some(vec![key.to_string()]),
                            // That one key, like the request view's map form
                            // (an embedding context lifts or drops the bound).
                            max_properties: Some(1),
                            ..Schema::object()
                        }))
                    })
                    .collect::<Vec<_>>()
            })
    };
    let Some(branches) = &schema.one_of else {
        return unit_keys(schema).map(|one_of| Schema {
            description: schema.description.clone(),
            one_of: Some(one_of),
            ..Schema::default()
        });
    };
    let mut changed = false;
    let one_of = branches
        .iter()
        .flat_map(|branch| match branch {
            SchemaRef::Inline(inline) => unit_keys(inline).map_or_else(
                || vec![branch.clone()],
                |keys| {
                    changed = true;
                    keys
                },
            ),
            SchemaRef::Ref(_) => vec![branch.clone()],
        })
        .collect();
    changed.then(|| Schema {
        one_of: Some(one_of),
        ..schema.clone()
    })
}

/// The key of a `{key: null}` single-key object branch (a unit variant's
/// map form).
fn null_key_branch(branch: &SchemaRef) -> Option<&str> {
    let SchemaRef::Inline(branch) = branch else {
        return None;
    };
    let mut properties = branch.properties.as_ref()?.iter();
    let (key, SchemaRef::Inline(value)) = properties.next()? else {
        return None;
    };
    (properties.next().is_none()
        && value.schema_type == Some(SchemaType::Null)
        && branch.required.as_deref() == Some(std::slice::from_ref(key)))
    .then_some(key.as_str())
}

/// The schema of the user type `ty` names (through `Box` / `Arc` / `Rc` /
/// `Cow` / `&`), built from its definition in the current view — for a
/// `$ref` that cannot be looked up by component name (a renamed component,
/// an `Input` view, an enum). `None` for anything else (generic instances,
/// unknown types).
pub(in crate::parser::schema) fn resolve_type(
    ty: &syn::Type,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<Schema> {
    let syn::Type::Path(type_path) = peel(ty) else {
        return None;
    };
    if !type_path.path.segments.last()?.arguments.is_none() {
        return None;
    }
    // `b::Foo` names the `Foo` module `b` declares: two renames may share
    // the ident, and the bare ident aliases only one of them (the same key
    // the `$ref` itself resolves through).
    let key =
        super::super::definition_key(&type_path.path, |key| struct_definitions.contains_key(key))?;
    resolve_name(&key, known_schemas, struct_definitions)
}

/// The schema of the definition stored under `name` in the current view. A
/// definition already being resolved yields an empty object: its keys are
/// already part of the object being built.
fn resolve_name(
    name: &str,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<Schema> {
    let definition = struct_definitions.get(name)?;
    if RESOLVING.with(|stack| stack.borrow().iter().any(|open| open == name)) {
        return Some(Schema::object_empty());
    }
    let item = syn::parse_str::<syn::Item>(definition.as_ref()).ok()?;
    RESOLVING.with(|stack| stack.borrow_mut().push(name.to_string()));
    let schema = match &item {
        syn::Item::Struct(item) => Some(parse_struct_to_schema(
            item,
            known_schemas,
            struct_definitions,
        )),
        syn::Item::Enum(item) => Some(parse_enum_to_schema(
            item,
            known_schemas,
            struct_definitions,
        )),
        _ => None,
    };
    RESOLVING.with(|stack| stack.borrow_mut().pop());
    schema
}

/// The type a `#[serde(flatten)]` `field` is documented through — its
/// `#[schema(value_type)]` (from `constraints`), else its declared type —
/// and whether it is an `Option` (seen through `&` / `Box` / `Arc` / `Rc` /
/// `Cow`, then its inner type is returned). Shared with `request_view`, so
/// the schema and the "needs a request view" check see the same member.
pub(in crate::parser::schema) fn flattened_member<'a>(
    field: &'a syn::Field,
    constraints: &'a SchemaConstraints,
) -> (&'a syn::Type, bool) {
    let declared = constraints
        .value_type
        .as_ref()
        .map_or(&field.ty, |ty| &ty.0);
    option_inner(peel(declared)).map_or((declared, false), |inner| (inner, true))
}

/// `ty` without the smart pointers / references serde sees through.
pub(in crate::parser::schema) fn peel(ty: &syn::Type) -> &syn::Type {
    match ty {
        syn::Type::Reference(syn::TypeReference { elem, .. })
        | syn::Type::Paren(syn::TypeParen { elem, .. })
        | syn::Type::Group(syn::TypeGroup { elem, .. }) => peel(elem),
        syn::Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .filter(|segment| {
                matches!(
                    segment.ident.to_string().as_str(),
                    "Box" | "Arc" | "Rc" | "Cow"
                )
            })
            .and_then(|segment| match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => {
                    args.args.iter().find_map(|arg| match arg {
                        syn::GenericArgument::Type(inner) => Some(peel(inner)),
                        _ => None,
                    })
                }
                _ => None,
            })
            .unwrap_or(ty),
        _ => ty,
    }
}

/// The mergeable parts of an object schema: an object (a struct, a map — its
/// `additionalProperties` value — or both), the `allOf: [<object>, ...]` form
/// [`assemble`] emits around other members (a member `$ref` whose definition
/// is known is merged too), or `null` (a unit struct, which has no keys).
/// `additionalProperties: false` is dropped (and remembered in `closed`): a
/// flattened struct only takes the keys it knows.
fn decompose(
    schema: &Schema,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<ObjectParts> {
    if schema.ref_path.is_some()
        || schema.one_of.is_some()
        || schema.any_of.is_some()
        || schema.not.is_some()
    {
        return None;
    }
    match schema.schema_type {
        Some(SchemaType::Null) => Some(ObjectParts::default()),
        Some(SchemaType::Object) => Some(ObjectParts {
            properties: schema.properties.clone().unwrap_or_default(),
            required: schema.required.clone().unwrap_or_default(),
            additional: match &schema.additional_properties {
                Some(AdditionalProperties::Schema(value)) => Some(value.clone()),
                _ => None,
            },
            closed: matches!(
                schema.additional_properties,
                Some(AdditionalProperties::Bool(false))
            ),
            // An object's own `allOf` only holds alias groups (see
            // `NamedFields::into_object_schema`).
            constraints: schema.all_of.clone().unwrap_or_default(),
            ..ObjectParts::default()
        }),
        None => {
            let (first, rest) = schema.all_of.as_deref()?.split_first()?;
            let mut parts = match first {
                // A newtype / transparent wrapper (or a described field)
                // around a component: that component's keys.
                SchemaRef::Ref(_) => referenced_parts(first, known_schemas, struct_definitions)?,
                SchemaRef::Inline(own) => (own.schema_type == Some(SchemaType::Object))
                    .then(|| decompose(own, known_schemas, struct_definitions))??,
            };
            for member in rest {
                match referenced_parts(member, known_schemas, struct_definitions) {
                    Some(member_parts) => {
                        merge(&mut parts, member_parts, false);
                    }
                    None => parts.opaque.push(member.clone()),
                }
            }
            Some(parts)
        }
        _ => None,
    }
}

/// The parts of the component a `$ref` member names, when its definition is
/// known (an `Input` component maps back to its source type).
fn referenced_parts(
    member: &SchemaRef,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<ObjectParts> {
    let SchemaRef::Ref(reference) = member else {
        return None;
    };
    let schema = resolve_reference(reference, known_schemas, struct_definitions)?;
    decompose(&schema, known_schemas, struct_definitions)
}

/// The schema of the component a `$ref` names, when its definition is known
/// (an `Input` component maps back to its source type; a recursive generic
/// instance is its materialised component).
pub(in crate::parser::schema) fn resolve_reference(
    reference: &vespera_core::schema::Reference,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<Schema> {
    let name = vespera_core::schema::schema_name_of_ref(&reference.ref_path)?;
    let source = request_view::component_of_input(&name).unwrap_or_else(|| name.clone());
    resolve_name(&source, known_schemas, struct_definitions)
        .or_else(|| generic_instances::materialized(&name))
}

/// Merge `parts` into `target`; `optional` parts (a flattened `Option`)
/// contribute their keys but require nothing. Returns whether `parts` had
/// members whose keys are unknown (they keep the object open).
fn merge(target: &mut ObjectParts, parts: ObjectParts, optional: bool) -> bool {
    for (key, schema) in parts.properties {
        target.properties.entry(key).or_insert(schema);
    }
    let open = !parts.opaque.is_empty();
    // An optional member is `None` when any of its keys fail, so it never
    // requires anything (its unknown members are dropped for the same
    // reason; the object stays open instead).
    if !optional {
        for key in parts.required {
            if !target.required.contains(&key) {
                target.required.push(key);
            }
        }
        target.constraints.extend(parts.constraints);
        target.opaque.extend(parts.opaque);
    }
    target.closed |= parts.closed;
    // Two flattened maps: serde READS every buffered entry into each map
    // (the value must fit both), but WRITES each map's own entries (the
    // value fits one of them).
    target.additional = match (target.additional.take(), parts.additional) {
        (Some(first), Some(second)) if request_view::in_request_view() => Some(both(first, second)),
        (Some(first), Some(second)) => Some(either(first, second)),
        (first, second) => first.or(second),
    };
    open
}

/// `allOf: [first, second]` — a value both schemas accept.
fn both(first: SchemaRef, second: SchemaRef) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        all_of: Some(vec![first, second]),
        ..Schema::default()
    }))
}

/// `anyOf: [first, second]` — a value one of the schemas accepts.
fn either(first: SchemaRef, second: SchemaRef) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        any_of: Some(vec![first, second]),
        ..Schema::default()
    }))
}

/// `anyOf: [schema, {}]` — documents `schema` without rejecting anything
/// (a flattened `Option` is `None` when its value fails).
fn lenient(schema: SchemaRef) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        any_of: Some(vec![schema, SchemaRef::Inline(Box::default())]),
        ..Schema::default()
    }))
}

/// The request-view adjustments of one merged member, in declaration order:
/// - an optional member (a flattened `Option`) is `None` whenever any of its
///   keys fail, so its properties (and map values) reject nothing;
/// - every non-optional map declared EARLIER also reads this member's keys
///   (it borrows the buffered entries before this member takes them), so
///   each of its properties must satisfy those maps' value schema too;
/// - `left`: the member leaves its keys in the buffer (see the module doc):
///   `later_maps` (the value schema of the non-optional maps declared
///   after it) read them too, and the outer `deny_unknown_fields`
///   (`rejected`) rejects them — every property is `{not: {}}`.
///
/// Returns the value schema later members must also satisfy.
fn request_member(
    parts: &mut ObjectParts,
    optional: bool,
    earlier_maps: Option<SchemaRef>,
    left: Option<(Option<SchemaRef>, bool)>,
) -> Option<SchemaRef> {
    let (later_maps, rejected) = left.unwrap_or_default();
    let maps = [earlier_maps.clone(), later_maps]
        .into_iter()
        .flatten()
        .reduce(both);
    for schema in parts.properties.values_mut() {
        if rejected {
            *schema = SchemaRef::Inline(Box::new(Schema {
                not: Some(SchemaRef::Inline(Box::default())),
                ..Schema::default()
            }));
            continue;
        }
        let own = std::mem::replace(schema, SchemaRef::Inline(Box::default()));
        let own = if optional { lenient(own) } else { own };
        *schema = match &maps {
            Some(map_value) => both(own, map_value.clone()),
            None => own,
        };
    }
    if optional {
        parts.additional = parts.additional.take().map(lenient);
        return earlier_maps;
    }
    match (earlier_maps, parts.additional.clone()) {
        (Some(first), Some(second)) => Some(both(first, second)),
        (first, second) => first.or(second),
    }
}

/// The object of `own` fields with the `flattened` members merged in.
///
/// A plain struct member (no map, nothing to close) keeps its `$ref` as an
/// `allOf` member, so its component — defaults and all — is reused. Every
/// other known member is merged; unknown members stay `allOf` members and
/// keep the object open (their keys cannot be listed).
///
/// `deny_unknown_fields` closes the merged object; with a flattened map the
/// request view is still closed (serde rejects every key no member
/// consumed, and a map consumes none), while the response view lists the
/// map's entries (serde writes them). In the request view an opaque member
/// that leaves its keys does not open the object under it either (serde
/// rejects them).
pub(super) fn assemble(
    mut own: ObjectParts,
    flattened: Vec<FlatPart>,
    description: Option<String>,
    deny_unknown_fields: bool,
) -> Schema {
    let request = request_view::in_request_view();
    let merge_all = deny_unknown_fields
        || flattened.iter().any(|part| {
            matches!(part, FlatPart::Known { parts, .. } if parts.additional.is_some() || parts.closed)
        });
    // The value schema of the non-optional maps declared after each member.
    let mut later_maps = Vec::with_capacity(flattened.len());
    let mut maps_after: Option<SchemaRef> = None;
    for part in flattened.iter().rev() {
        later_maps.push(maps_after.clone());
        if let FlatPart::Known {
            parts:
                ObjectParts {
                    additional: Some(value),
                    ..
                },
            optional: false,
            ..
        } = part
        {
            maps_after =
                Some(maps_after.map_or_else(|| value.clone(), |after| both(value.clone(), after)));
        }
    }
    let mut open = false;
    let mut earlier_maps: Option<SchemaRef> = None;
    for (part, later) in flattened.into_iter().zip(later_maps.into_iter().rev()) {
        match part {
            FlatPart::Opaque { schema, leaves } => {
                open |= !(request && deny_unknown_fields && leaves);
                own.opaque.push(schema);
            }
            // Its keys are known, so it does not open the object.
            FlatPart::Known {
                reference: Some(reference),
                optional: false,
                ..
            } if !merge_all => own.opaque.push(reference),
            FlatPart::Known {
                mut parts,
                optional,
                leaves,
                ..
            } => {
                // serde rejects every key such a member leaves behind, its
                // unknown members' keys too: they do not open the object.
                let rejected = request && deny_unknown_fields && leaves;
                if request {
                    let left = leaves.then_some((later, deny_unknown_fields));
                    earlier_maps = request_member(&mut parts, optional, earlier_maps, left);
                }
                open |= merge(&mut own, parts, optional) && !rejected;
            }
        }
    }
    let additional_properties = if open {
        None
    } else if deny_unknown_fields && (request || own.additional.is_none()) {
        Some(AdditionalProperties::Bool(false))
    } else {
        own.additional.map(AdditionalProperties::Schema)
    };
    let object = Schema {
        schema_type: Some(SchemaType::Object),
        properties: (!own.properties.is_empty()).then_some(own.properties),
        required: (!own.required.is_empty()).then_some(own.required),
        additional_properties,
        all_of: (!own.constraints.is_empty()).then_some(own.constraints),
        ..Schema::object_empty()
    };
    if own.opaque.is_empty() {
        return Schema {
            description,
            ..object
        };
    }
    let mut all_of = vec![SchemaRef::Inline(Box::new(object))];
    all_of.extend(own.opaque);
    Schema {
        description,
        all_of: Some(all_of),
        ..Schema::default()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// `resolve_type` sees through the wrappers serde sees through, and
    /// gives up on anything that is not a plain struct / enum definition.
    #[rstest]
    #[case::paren("(Page)", true)]
    #[case::cow_with_lifetime("Cow<'static, Page>", true)]
    #[case::bare_box("Box", false)]
    #[case::type_alias("Alias", false)]
    #[case::tuple("(Page, Page)", false)]
    fn resolve_type_peels_wrappers(#[case] ty: &str, #[case] resolved: bool) {
        let known = HashSet::from(["Page".to_string(), "Alias".to_string()]);
        let definitions = HashMap::from([
            ("Page".to_string(), "struct Page { page: u32 }".to_string()),
            ("Alias".to_string(), "type Alias = Page;".to_string()),
        ]);
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        assert_eq!(resolve_type(&ty, &known, &definitions).is_some(), resolved);
    }

    /// A flattened unit struct reads and writes no keys: it contributes
    /// nothing (never an `allOf` member `$ref` to its `null` component).
    #[test]
    fn flattened_unit_struct_contributes_nothing() {
        let known = HashSet::from(["Marker".to_string()]);
        let definitions = HashMap::from([("Marker".to_string(), "struct Marker;".to_string())]);
        let item: syn::ItemStruct =
            syn::parse_str("struct S { id: u8, #[serde(flatten)] marker: Marker }").unwrap();
        let schema = parse_struct_to_schema(&item, &known, &definitions);
        insta::assert_snapshot!(
            "flatten_unit_struct",
            serde_json::to_string(&schema).unwrap()
        );
    }

    /// A flattened member serde may leave out of the output
    /// (`skip_serializing_if`) requires none of its keys in the response view
    /// (an opaque member becomes lenient); one it never writes
    /// (`skip_serializing`) adds no keys there at all. The request view still
    /// requires them.
    #[rstest]
    #[case::skip_serializing_if("if", r#"skip_serializing_if = "Meta::is_empty""#, "Meta")]
    #[case::skip_serializing("always", "skip_serializing", "Meta")]
    #[case::opaque_member("opaque", r#"skip_serializing_if = "Kind::is_none""#, "Kind")]
    fn omittable_flattened_member_requires_nothing_on_output(
        #[case] name: &str,
        #[case] skip: &str,
        #[case] member: &str,
    ) {
        let known = HashSet::from(["Meta".to_string(), "Kind".to_string()]);
        let definitions = HashMap::from([
            ("Meta".to_string(), "struct Meta { by: String }".to_string()),
            (
                "Kind".to_string(),
                "#[serde(untagged)] enum Kind { A { a: u8 } }".to_string(),
            ),
        ]);
        let item: syn::ItemStruct = syn::parse_str(&format!(
            "struct Resp {{ id: u32, #[serde(flatten, {skip})] meta: {member} }}"
        ))
        .unwrap();
        let schema = parse_struct_to_schema(&item, &known, &definitions);
        let request =
            request_view::with_request_view(|| parse_struct_to_schema(&item, &known, &definitions));
        insta::assert_snapshot!(
            format!("flatten_omittable_{name}"),
            format!(
                "response: {}\nrequest: {}",
                serde_json::to_string(&schema).unwrap(),
                serde_json::to_string(&request).unwrap()
            )
        );
    }

    /// serde writes a flattened externally tagged unit variant as the key
    /// `{"A": null}` (`FlatMapSerializer::serialize_unit_variant`), never
    /// as the string `"A"`.
    #[rstest]
    #[case::mixed("mixed", "struct S { id: u8, #[serde(flatten)] kind: Kind }")]
    #[case::unit_only("unit_only", "struct S { #[serde(flatten)] mode: Option<Mode> }")]
    fn flattened_unit_variants_are_keys(#[case] name: &str, #[case] src: &str) {
        let known = HashSet::from(["Kind".to_string(), "Mode".to_string()]);
        let definitions = HashMap::from([
            (
                "Kind".to_string(),
                "enum Kind { A, #[serde(alias = \"bee\")] B { x: u8 } }".to_string(),
            ),
            (
                "Mode".to_string(),
                "enum Mode { On, #[serde(rename = \"off\")] Off }".to_string(),
            ),
        ]);
        let item: syn::ItemStruct = syn::parse_str(src).unwrap();
        let schema = parse_struct_to_schema(&item, &known, &definitions);
        insta::assert_snapshot!(
            format!("flatten_unit_variants_{name}"),
            serde_json::to_string_pretty(&schema).unwrap()
        );
    }

    /// Only a `{key: null}` object with that one required key is a unit
    /// variant's map form.
    #[test]
    fn null_key_branches_are_single_null_keys() {
        let branch = |value: serde_json::Value| -> SchemaRef {
            serde_json::from_value(value).expect("hand-built branch deserializes")
        };
        let keyed = |key: &str, value: serde_json::Value| {
            branch(serde_json::json!({
                "type": "object", "properties": {key: value}, "required": [key]
            }))
        };
        assert_eq!(
            null_key_branch(&keyed("A", serde_json::json!({"type": "null"}))),
            Some("A")
        );
        let not_null_keys = [
            branch(serde_json::json!({"$ref": "#/components/schemas/Payload"})),
            branch(serde_json::json!({"type": "string", "enum": ["A"]})),
            keyed(
                "A",
                serde_json::json!({"$ref": "#/components/schemas/Payload"}),
            ),
            keyed("A", serde_json::json!({"type": "integer"})),
            branch(serde_json::json!({
                "type": "object",
                "properties": {"A": {"type": "null"}, "B": {"type": "null"}},
                "required": ["A"]
            })),
            branch(serde_json::json!({"type": "object", "properties": {"A": {"type": "null"}}})),
            branch(serde_json::json!({"type": "object", "properties": {}})),
        ];
        for branch in &not_null_keys {
            assert_eq!(null_key_branch(branch), None, "{branch:?}");
        }
    }

    /// A `$ref` branch (a newtype variant over a component) is kept as is
    /// next to the converted unit names.
    #[test]
    fn flattened_unit_variants_keep_ref_branches() {
        let schema = Schema {
            one_of: Some(vec![
                SchemaRef::Ref(vespera_core::schema::Reference::schema("Payload")),
                SchemaRef::Inline(Box::new(Schema {
                    r#enum: Some(vec![serde_json::json!("A")]),
                    ..Schema::string()
                })),
            ]),
            ..Schema::default()
        };
        let flattened = flattened_unit_variants(&schema).unwrap();
        assert_eq!(
            serde_json::to_value(&flattened).unwrap()["oneOf"],
            serde_json::json!([
                {"$ref": "#/components/schemas/Payload"},
                {
                    "type": "object",
                    "properties": {"A": {"type": "null"}},
                    "required": ["A"],
                    "maxProperties": 1
                }
            ])
        );
    }

    /// Only a path naming a known definition that serde reads without
    /// taking its keys leaves them behind.
    #[rstest]
    #[case::tuple("(Meta, u8)", false)]
    #[case::unknown("Ghost", false)]
    #[case::alias("Alias", false)]
    #[case::leaving("Meta", true)]
    #[case::newtype("Wrapped", true)]
    fn leaves_keys_needs_a_known_leaving_definition(#[case] ty: &str, #[case] leaves: bool) {
        let definitions = HashMap::from([
            (
                "Audit".to_string(),
                "struct Audit { by: String }".to_string(),
            ),
            (
                "Meta".to_string(),
                "struct Meta { #[serde(flatten)] audit: Audit }".to_string(),
            ),
            ("Wrapped".to_string(), "struct Wrapped(Meta);".to_string()),
            ("Alias".to_string(), "type Alias = Meta;".to_string()),
        ]);
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        assert_eq!(leaves_keys(&ty, &definitions, WRAPPER_DEPTH), leaves);
    }

    /// A macro-expanded `$ty` arrives as an invisible group.
    #[test]
    fn resolve_type_peels_invisible_groups() {
        let known = HashSet::from(["Page".to_string()]);
        let definitions =
            HashMap::from([("Page".to_string(), "struct Page { page: u32 }".to_string())]);
        let ty = syn::Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(syn::parse_str("Page").unwrap()),
        });
        assert!(resolve_type(&ty, &known, &definitions).is_some());
    }

    /// An `allOf` that does not start with an inline object (a described
    /// `$ref`) has no mergeable keys; an inline non-`$ref` member after the
    /// object stays an opaque member.
    #[test]
    fn decompose_all_of_members() {
        let known = HashSet::<String>::new();
        let definitions = HashMap::<String, String>::new();
        let reference = SchemaRef::Ref(vespera_core::schema::Reference::schema("Page"));
        let described_ref = Schema {
            all_of: Some(vec![reference]),
            ..Schema::default()
        };
        assert!(decompose(&described_ref, &known, &definitions).is_none());

        let member = SchemaRef::Inline(Box::new(Schema::string()));
        let object_then_inline = Schema {
            all_of: Some(vec![
                SchemaRef::Inline(Box::new(Schema::object())),
                member.clone(),
            ]),
            ..Schema::default()
        };
        let parts = decompose(&object_then_inline, &known, &definitions).unwrap();
        assert_eq!(parts.opaque.len(), 1);
        assert!(matches!(
            &parts.opaque[0],
            SchemaRef::Inline(schema) if schema.schema_type == Some(SchemaType::String)
        ));
    }
}
