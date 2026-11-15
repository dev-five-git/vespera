//! Schemas embedded in an object holding keys serde never shows them.
//!
//! serde hands a `#[serde(flatten)]` member only the keys the outer struct
//! left in its buffer, and an internally tagged newtype payload the map
//! without the tag. JSON Schema checks such a member (an `allOf` member)
//! against the WHOLE object, so every object inside it that constrains the
//! keys it does not list — `deny_unknown_fields` (`additionalProperties:
//! false`), a map (`additionalProperties: <value>`), `minProperties` /
//! `maxProperties` (a map's `length`, an externally tagged variant's single
//! key), `propertyNames` (the `#[serde(other)]` map form's unknown key) —
//! would reject the keys serde keeps from it. [`open_embedded`]
//! reopens those objects wherever they sit: on the schema itself, in every
//! `oneOf` / `anyOf` / `allOf` branch, and through the `$ref`s of nested
//! components, newtype / transparent wrappers and recursive generic
//! instances (a `$ref` is inlined only when its copy changed). The caller
//! decides how an object is reopened: a flattened member drops the
//! constraints ([`without_unlisted_bounds`]), a tagged payload lists the tag.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{AdditionalProperties, Reference, Schema, SchemaRef};

use super::struct_schema::{flattened_unit_variants, resolve_reference};

/// Whether `schema` constrains the keys it does not list itself.
pub(super) fn constrains_unlisted(schema: &Schema) -> bool {
    matches!(
        schema.additional_properties,
        Some(AdditionalProperties::Bool(false) | AdditionalProperties::Schema(_))
    ) || schema.min_properties.is_some()
        || schema.max_properties.is_some()
        || schema.property_names.is_some()
}

/// `schema` without its constraints on unlisted keys (see
/// [`constrains_unlisted`]); `None` when it has none.
pub(super) fn without_unlisted_bounds(schema: &Schema) -> Option<Schema> {
    constrains_unlisted(schema).then(|| Schema {
        additional_properties: None,
        min_properties: None,
        max_properties: None,
        property_names: None,
        ..schema.clone()
    })
}

/// The schema a newtype / transparent wrapper (`allOf: [$ref X]`, nothing
/// else but a description) stands for: `X`'s resolved schema, through any
/// number of wrappers (the outermost description wins). serde reads and
/// writes a wrapper exactly like the value it wraps.
pub(super) fn unwrap_wrappers(
    mut schema: Schema,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut seen: Vec<String> = Vec::new();
    while let Some(reference) = sole_reference(&schema)
        && !seen.contains(&reference.ref_path)
        && let Some(inner) = resolve_reference(reference, known_schemas, struct_definitions)
    {
        seen.push(reference.ref_path.clone());
        let description = schema.description.take().or(inner.description.clone());
        schema = Schema {
            description,
            ..inner
        };
    }
    schema
}

/// The `$ref` of a wrapper schema (`allOf: [$ref]` plus at most a
/// description).
fn sole_reference(schema: &Schema) -> Option<&Reference> {
    let [SchemaRef::Ref(reference)] = schema.all_of.as_deref()? else {
        return None;
    };
    let rest = Schema {
        all_of: None,
        description: None,
        ..schema.clone()
    };
    // `nullable` serializes beside the `allOf` (as an `anyOf` with `null`).
    (rest.nullable.is_none() && serde_json::to_value(&rest).ok()? == serde_json::json!({}))
        .then_some(reference)
}

/// `schema` with `reopen` applied to itself and to every object it embeds
/// (see the module docs); `None` when nothing changed. `reopen` returns the
/// reopened copy of an object, or `None` to keep it.
pub(super) fn open_embedded(
    schema: &Schema,
    reopen: &dyn Fn(&Schema) -> Option<Schema>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<Schema> {
    open(
        schema,
        reopen,
        known_schemas,
        struct_definitions,
        &mut Vec::new(),
    )
}

/// [`open_embedded`] with the `$ref`s being opened (a recursive component
/// reached again keeps its `$ref`).
fn open(
    schema: &Schema,
    reopen: &dyn Fn(&Schema) -> Option<Schema>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
    opening: &mut Vec<String>,
) -> Option<Schema> {
    let reopened = reopen(schema);
    let mut changed = reopened.is_some();
    let mut schema = reopened.unwrap_or_else(|| schema.clone());
    for branches in [&mut schema.one_of, &mut schema.any_of, &mut schema.all_of]
        .into_iter()
        .flatten()
    {
        for branch in branches {
            let opened = match branch {
                SchemaRef::Inline(inline) => {
                    open(inline, reopen, known_schemas, struct_definitions, opening)
                }
                SchemaRef::Ref(reference) => open_reference(
                    reference,
                    reopen,
                    known_schemas,
                    struct_definitions,
                    opening,
                ),
            };
            if let Some(opened) = opened {
                *branch = SchemaRef::Inline(Box::new(opened));
                changed = true;
            }
        }
    }
    changed.then_some(schema)
}

/// The opened copy of the component `reference` names; `None` when it is
/// unknown, already being opened, or needs no change.
fn open_reference(
    reference: &Reference,
    reopen: &dyn Fn(&Schema) -> Option<Schema>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
    opening: &mut Vec<String>,
) -> Option<Schema> {
    if opening.contains(&reference.ref_path) {
        return None;
    }
    let resolved = resolve_reference(reference, known_schemas, struct_definitions)?;
    opening.push(reference.ref_path.clone());
    // The embedded value is a map, so a nested externally tagged enum's unit
    // variant is its key (`{"A": null}`), never the string `"A"`.
    let keyed = flattened_unit_variants(&resolved);
    let opened = open(
        keyed.as_ref().unwrap_or(&resolved),
        reopen,
        known_schemas,
        struct_definitions,
        opening,
    )
    .or(keyed);
    opening.pop();
    opened
}

#[cfg(test)]
mod tests;
