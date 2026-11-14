//! `#[serde(flatten)]` members merged into the enclosing object.
//!
//! serde's derive (serde 1.0.229, `serde_derive::de::struct_`) buffers every
//! key the outer struct does not know itself, then deserializes each
//! flattened member from that buffer:
//!
//! - a flattened STRUCT takes only the keys it knows
//!   (`FlatStructAccess`), so its own `deny_unknown_fields` never fires —
//!   its fields are simply more keys of the outer object;
//! - a flattened MAP reads every key still in the buffer
//!   (`FlatMapAccess`), so it types the outer object's unlisted keys;
//! - a flattened `Option<T>` is `None` whenever `T` fails, so it never makes
//!   a key required;
//! - a flattened unit struct reads nothing;
//! - the OUTER `deny_unknown_fields` rejects any key no member consumed
//!   (a map borrows, never consumes, so with it the map stays empty).
//!
//! JSON Schema `additionalProperties` only sees the `properties` of its own
//! schema object, so composing members with `allOf` gets these semantics
//! wrong (an outer `{"id": 1}` would be checked against a flattened map's
//! value schema, and a `deny_unknown_fields` member would reject every outer
//! key). Members whose keys are known are therefore MERGED into one object;
//! only members whose keys cannot be known (a flattened enum, an
//! unresolvable type) stay `allOf` members — and they keep the merged object
//! open.

use std::{
    borrow::Borrow,
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{AdditionalProperties, Schema, SchemaRef, SchemaType};

use super::{
    super::{
        enum_schema::parse_enum_to_schema,
        request_view,
        schema_attrs::extract_schema_constraints,
        serde_attrs::{SerdeSide, extract_field_codec},
        type_schema::parse_type_to_schema_ref,
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

/// One `#[serde(flatten)]` field.
pub(super) enum FlatPart {
    /// A member whose keys are known; `reference` is its `$ref`, if any.
    Known {
        reference: Option<SchemaRef>,
        parts: ObjectParts,
        optional: bool,
    },
    /// A member whose keys cannot be known.
    Opaque(SchemaRef),
}

/// Classify a `#[serde(flatten)]` field on `side`.
pub(super) fn flatten_part(
    field: &syn::Field,
    side: SerdeSide,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> FlatPart {
    let constraints = extract_schema_constraints(&field.attrs);
    let declared = constraints
        .value_type
        .as_ref()
        .map_or(&field.ty, |ty| &ty.0);
    let (ty, optional) = option_inner(declared).map_or((declared, false), |inner| (inner, true));
    let custom = constraints.is_any() || extract_field_codec(&field.attrs).custom_for(side);
    let schema = if custom {
        SchemaRef::Inline(Box::default())
    } else {
        parse_type_to_schema_ref(ty, known_schemas, struct_definitions)
    };
    let (resolved, reference) = match &schema {
        SchemaRef::Inline(inline) => (Some((**inline).clone()), None),
        SchemaRef::Ref(_) => (
            resolve_type(ty, known_schemas, struct_definitions),
            Some(schema.clone()),
        ),
    };
    match resolved
        .as_ref()
        .and_then(|schema| decompose(schema, known_schemas, struct_definitions))
    {
        Some(parts) => FlatPart::Known {
            reference,
            parts,
            optional,
        },
        // serde turns any failure of the inner value into `None`.
        None if optional => FlatPart::Opaque(SchemaRef::Inline(Box::new(Schema {
            any_of: Some(vec![schema, SchemaRef::Inline(Box::default())]),
            ..Schema::default()
        }))),
        None => FlatPart::Opaque(schema),
    }
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
    let segment = type_path.path.segments.last()?;
    if !segment.arguments.is_none() {
        return None;
    }
    resolve_name(
        &segment.ident.to_string(),
        known_schemas,
        struct_definitions,
    )
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

/// `ty` without the smart pointers / references serde sees through.
fn peel(ty: &syn::Type) -> &syn::Type {
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
            let SchemaRef::Inline(own) = first else {
                return None;
            };
            let mut parts = (own.schema_type == Some(SchemaType::Object))
                .then(|| decompose(own, known_schemas, struct_definitions))??;
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
    let name = reference.ref_path.strip_prefix("#/components/schemas/")?;
    let name = request_view::component_of_input(name).unwrap_or_else(|| name.to_string());
    let schema = resolve_name(&name, known_schemas, struct_definitions)?;
    decompose(&schema, known_schemas, struct_definitions)
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
    target.additional = match (target.additional.take(), parts.additional) {
        (Some(first), Some(second)) => Some(SchemaRef::Inline(Box::new(Schema {
            all_of: Some(vec![first, second]),
            ..Schema::default()
        }))),
        (first, second) => first.or(second),
    };
    open
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
/// map's entries (serde writes them).
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
    let mut open = false;
    for part in flattened {
        match part {
            FlatPart::Opaque(schema) => {
                open = true;
                own.opaque.push(schema);
            }
            // Its keys are known, so it does not open the object.
            FlatPart::Known {
                reference: Some(reference),
                optional: false,
                ..
            } if !merge_all => own.opaque.push(reference),
            FlatPart::Known {
                parts, optional, ..
            } => open |= merge(&mut own, parts, optional),
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
}
