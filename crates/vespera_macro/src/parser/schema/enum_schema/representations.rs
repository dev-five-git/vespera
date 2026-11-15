use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{AdditionalProperties, Discriminator, Schema, SchemaRef, SchemaType};

use super::super::{
    request_view::in_request_view,
    schema_attrs::extract_schema_constraints,
    serde_attrs::{extract_doc_comment, extract_field_codec},
    struct_schema::{flattened_unit_variants, resolve_type, tuple_fields_schema, with_description},
};
use super::{
    EnumRenames,
    unit::{other_tag_schema, variant_keys},
    variant::{
        build_struct_variant_fields, build_variant_data_schema, is_other_variant,
        is_variant_omitted, newtype_field_schema, variant_codec_schema,
    },
};
use crate::schema_macro::type_utils::{is_option_type, option_inner};

/// Variants that take part in the wire contract (not `skip` /
/// `skip_serializing`).
fn emitted_variants(enum_item: &syn::ItemEnum) -> impl Iterator<Item = &syn::Variant> {
    enum_item
        .variants
        .iter()
        .filter(|variant| !is_variant_omitted(variant))
}

/// `{type: string, enum: keys}` — the tag values of one variant.
fn tag_value_schema(keys: Vec<String>) -> Schema {
    Schema {
        r#enum: Some(keys.into_iter().map(serde_json::Value::String).collect()),
        ..Schema::string()
    }
}

/// Every name a non-`other` emitted variant is identified by (aliases
/// included in the request view).
fn known_keys(enum_item: &syn::ItemEnum, rename_all: Option<&str>) -> Vec<serde_json::Value> {
    emitted_variants(enum_item)
        .filter(|variant| !is_other_variant(variant))
        .flat_map(|variant| variant_keys(variant, rename_all))
        .map(serde_json::Value::String)
        .collect()
}

/// Every emitted variant of a tagged enum paired with its tag-value schema
/// (in declaration order). In the request view the `#[serde(other)]`
/// catch-all accepts any string that is NOT one of the other variants' tags
/// (`{type: string, not: {enum: [...]}}`), which keeps the `oneOf` branches
/// mutually exclusive; serde writes it under its own name, so the response
/// view lists just that.
fn tagged_variants<'a>(
    enum_item: &'a syn::ItemEnum,
    rename_all: Option<&str>,
) -> Vec<(&'a syn::Variant, SchemaRef)> {
    let request = in_request_view();
    let known_tags = known_keys(enum_item, rename_all);
    emitted_variants(enum_item)
        .map(|variant| {
            let schema = if request && is_other_variant(variant) {
                SchemaRef::Inline(Box::new(other_tag_schema(known_tags.clone())))
            } else {
                SchemaRef::Inline(Box::new(tag_value_schema(variant_keys(
                    variant, rename_all,
                ))))
            };
            (variant, schema)
        })
        .collect()
}

fn non_empty<T>(items: Vec<T>) -> Option<Vec<T>> {
    (!items.is_empty()).then_some(items)
}

/// Parse externally tagged enum: `{"VariantName": {...}}`
/// This is serde's default representation.
///
/// In the request view each `#[serde(alias)]` of a data variant is one more
/// `{"alias": <data>}` branch, and a `#[serde(other)]` variant accepts any
/// string that names no other variant.
pub(super) fn parse_externally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    renames: EnumRenames<'_>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());
    let request = in_request_view();

    for variant in emitted_variants(enum_item) {
        let variant_description = extract_doc_comment(&variant.attrs);
        if request && is_other_variant(variant) {
            one_of_schemas.push(SchemaRef::Inline(Box::new(Schema {
                description: variant_description,
                ..other_tag_schema(known_keys(enum_item, renames.variants))
            })));
            continue;
        }
        let keys = variant_keys(variant, renames.variants);

        if let Some(data_schema) =
            build_variant_data_schema(variant, renames, known_schemas, struct_definitions)
        {
            // Tuple / struct variant: {"VariantName": <data>} — exactly
            // that one key: serde_json's `deserialize_enum` rejects anything
            // after the variant entry (see `without_single_key_bounds` for
            // the contexts that merge more keys next to it).
            for key in keys {
                one_of_schemas.push(SchemaRef::Inline(Box::new(Schema {
                    description: variant_description.clone(),
                    properties: Some(BTreeMap::from([(key.clone(), data_schema.clone())])),
                    required: Some(vec![key]),
                    max_properties: Some(1),
                    ..Schema::object()
                })));
            }
        } else {
            // Unit variant in mixed enum: string with const value
            one_of_schemas.push(SchemaRef::Inline(Box::new(Schema {
                description: variant_description.clone(),
                ..tag_value_schema(keys.clone())
            })));
            // serde_json also reads the map form `{"Name": null}`
            // (`VariantAccess::unit_variant` deserializes `()` from the
            // entry's value).
            if request {
                one_of_schemas.extend(keys.into_iter().map(|key| {
                    SchemaRef::Inline(Box::new(Schema {
                        description: variant_description.clone(),
                        properties: Some(BTreeMap::from([(
                            key.clone(),
                            SchemaRef::Inline(Box::new(Schema::new(SchemaType::Null))),
                        )])),
                        required: Some(vec![key]),
                        max_properties: Some(1),
                        ..Schema::object()
                    }))
                }));
            }
        }
    }

    Schema {
        schema_type: None,
        description,
        one_of: non_empty(one_of_schemas),
        ..Schema::new(SchemaType::Object)
    }
}

/// `schema` without the `maxProperties: 1` of its externally tagged data
/// branches (see [`parse_externally_tagged_enum`]), for a context where
/// serde reads the enum from a map holding other keys too — a `flatten`ed
/// enum (`FlatMapDeserializer` picks the variant key out of the buffered
/// map) or an internally tagged newtype payload (next to the tag). `None`
/// when there is no such branch.
pub(in crate::parser::schema) fn without_single_key_bounds(schema: &Schema) -> Option<Schema> {
    let branches = schema.one_of.as_ref()?;
    let mut changed = false;
    let one_of = branches
        .iter()
        .map(|branch| match branch {
            SchemaRef::Inline(inline) if inline.max_properties == Some(1) => {
                changed = true;
                SchemaRef::Inline(Box::new(Schema {
                    max_properties: None,
                    ..(**inline).clone()
                }))
            }
            other => other.clone(),
        })
        .collect();
    changed.then(|| Schema {
        one_of: Some(one_of),
        ..schema.clone()
    })
}

/// The object schema of an internally tagged newtype payload that constrains
/// unlisted properties — closed (`#[serde(deny_unknown_fields)]`,
/// `additionalProperties: false`) or a map (`additionalProperties: <value>`)
/// — which would reject the tag inside an `allOf`. `None` for an open
/// payload.
fn closed_payload_object(payload: Schema) -> Option<Schema> {
    matches!(
        payload.additional_properties,
        Some(AdditionalProperties::Bool(false) | AdditionalProperties::Schema(_))
    )
    .then_some(payload)
}

/// `object` with the required `tag` property (first in `required`); the
/// variant doc comment, when present, replaces the payload's description.
fn with_tag_property(
    mut object: Schema,
    tag: &str,
    tag_schema: &SchemaRef,
    description: Option<String>,
) -> Schema {
    object
        .properties
        .get_or_insert_with(BTreeMap::new)
        .insert(tag.to_string(), tag_schema.clone());
    object
        .required
        .get_or_insert_with(Vec::new)
        .insert(0, tag.to_string());
    if description.is_some() {
        object.description = description;
    }
    // The tag is one more property of the object (a map payload's
    // `length` bounds count its entries only).
    object.min_properties = object.min_properties.map(|min| min + 1);
    object.max_properties = object.max_properties.map(|max| max + 1);
    object
}

/// Parse internally tagged enum: `{"tag": "VariantName", ...fields...}`
/// Uses `OpenAPI` discriminator for the tag field.
///
/// serde allows unit, struct and newtype variants here (multi-field tuple
/// variants are a serde compile error). A newtype variant's inner value must
/// serialize as a map, so it is emitted as
/// `allOf: [{tag object}, <inner schema>]` — except over a closed or map
/// payload (`additionalProperties: false | <value>`), which would reject the
/// tag: the tag is merged into a copy of the payload object instead. A unit
/// payload (`()`, a unit struct) serializes as just the tag, and an
/// externally tagged enum payload keeps its variant key next to the tag (a
/// unit variant as `{"A": null}`). Non-object inner types are rejected at
/// `#[derive(Schema)]` time when the syntax tells (see
/// `validate_enum_for_schema`), else once every component is known
/// (`reject_internally_tagged_non_object_refs`).
#[allow(clippy::too_many_lines)]
pub(super) fn parse_internally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    renames: EnumRenames<'_>,
    tag: &str,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());
    let tag_string = tag.to_string();
    for (variant, tag_schema) in tagged_variants(enum_item, renames.variants) {
        let variant_description = extract_doc_comment(&variant.attrs);
        let tag_object = |description: Option<String>| Schema {
            description,
            properties: Some(BTreeMap::from([(tag_string.clone(), tag_schema.clone())])),
            required: Some(vec![tag_string.clone()]),
            ..Schema::object()
        };
        let tag_and = |payload: SchemaRef, description: Option<String>| Schema {
            description,
            all_of: Some(vec![SchemaRef::Inline(Box::new(tag_object(None))), payload]),
            ..Schema::default()
        };
        // A variant-level `with` hides the payload's shape.
        let variant_schema = match (variant_codec_schema(variant), &variant.fields) {
            // Unit variant: {"tag": "VariantName"}
            (_, syn::Fields::Unit) => tag_object(variant_description),
            (Some(codec), _) => tag_and(codec, variant_description),
            (None, syn::Fields::Named(fields_named)) => {
                // Struct variant: {"tag": "VariantName", field1: type1, ...}
                let mut fields = build_struct_variant_fields(
                    fields_named,
                    renames.fields,
                    &variant.attrs,
                    known_schemas,
                    struct_definitions,
                );
                fields
                    .properties
                    .insert(tag_string.clone(), tag_schema.clone());
                fields.required.insert(0, tag_string.clone());
                fields.into_object_schema(variant_description, renames.deny_unknown_fields)
            }
            (None, syn::Fields::Unnamed(fields_unnamed)) if fields_unnamed.unnamed.len() == 1 => {
                // serde never writes an `Option` payload's `None` here
                // (`TaggedSerializer::serialize_none` errors) and reads the
                // tagged map as `Some`: the payload is the inner type's.
                let declared = &fields_unnamed.unnamed[0];
                let peeled = option_inner(&declared.ty).map(|inner| syn::Field {
                    ty: inner.clone(),
                    ..declared.clone()
                });
                let field = peeled.as_ref().unwrap_or(declared);
                let inner = newtype_field_schema(field, known_schemas, struct_definitions);
                // A `$ref` payload is resolved from its Rust type (a
                // request-view `$ref` names the `Input` component, so the
                // `$ref` itself cannot be looked up) — the `value_type`, when
                // present, since that is what `inner` documents.
                let payload = match &inner {
                    SchemaRef::Inline(schema) => Some((**schema).clone()),
                    SchemaRef::Ref(_) => {
                        let value_type = extract_schema_constraints(&field.attrs).value_type;
                        let declared = value_type.as_ref().map_or(&field.ty, |ty| &ty.0);
                        resolve_type(declared, known_schemas, struct_definitions)
                    }
                };
                // An externally tagged enum payload is read from the map left
                // once the tag is taken out: its variant key sits next to the
                // tag, and serde writes a unit variant as that key
                // (`TaggedSerializer::serialize_unit_variant`: `{tag, "A": null}`).
                let open_enum = payload.as_ref().and_then(|payload| {
                    let open = without_single_key_bounds(payload);
                    flattened_unit_variants(open.as_ref().unwrap_or(payload)).or(open)
                });
                if let Some(open) = open_enum {
                    tag_and(SchemaRef::Inline(Box::new(open)), variant_description)
                } else {
                    match payload.as_ref().and_then(|payload| payload.schema_type) {
                        // Newtype of `()` / a unit struct serializes as just
                        // the tag, and serde reads the payload only from an
                        // EMPTY remaining map
                        // (`ContentDeserializer::deserialize_unit`).
                        Some(SchemaType::Null) => Schema {
                            additional_properties: Some(AdditionalProperties::Bool(false)),
                            ..tag_object(variant_description)
                        },
                        // A map-like (or referenced struct) inner value carries
                        // the tag: a closed payload object as one of its own
                        // properties.
                        None | Some(SchemaType::Object) => {
                            match payload.and_then(closed_payload_object) {
                                Some(object) => with_tag_property(
                                    object,
                                    &tag_string,
                                    &tag_schema,
                                    variant_description,
                                ),
                                None => tag_and(inner, variant_description),
                            }
                        }
                        // A non-object payload (a `$ref` to a non-object
                        // component, an inline `Uuid` string) cannot carry the
                        // tag in serde: it is kept as `allOf: [{tag}, <payload>]`
                        // and reported once every component is known (see
                        // `validate_enum_for_schema`).
                        Some(_) => tag_and(inner, variant_description),
                    }
                }
            }
            // Multi-field tuple variants are a serde compile error here.
            (None, syn::Fields::Unnamed(_)) => continue,
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    // Only multi-field tuple variants (a serde compile error): nothing is
    // accepted, and a `discriminator` without `oneOf` is invalid.
    let Some(one_of) = non_empty(one_of_schemas) else {
        return Schema {
            description,
            not: Some(SchemaRef::Inline(Box::default())),
            ..Schema::default()
        };
    };
    Schema {
        schema_type: None,
        description,
        one_of: Some(one_of),
        discriminator: Some(Discriminator {
            property_name: tag_string,
            mapping: None, // Mapping not needed for inline schemas
        }),
        ..Default::default()
    }
}

/// Parse adjacently tagged enum: `{"tag": "VariantName", "content": {...}}`
/// Uses `OpenAPI` discriminator for the tag field.
pub(super) fn parse_adjacently_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    renames: EnumRenames<'_>,
    tag: &str,
    content: &str,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());
    let tag_string = tag.to_string();
    let content_string = content.to_string();
    for (variant, tag_schema) in tagged_variants(enum_item, renames.variants) {
        let variant_description = extract_doc_comment(&variant.attrs);

        let mut properties = BTreeMap::new();
        let mut required = vec![tag_string.clone()];
        properties.insert(tag_string.clone(), tag_schema);

        // Add the content field if variant has data
        if let Some(data_schema) =
            build_variant_data_schema(variant, renames, known_schemas, struct_definitions)
        {
            properties.insert(content_string.clone(), data_schema);
            // serde reads a missing content key of a newtype variant
            // (without a variant `deserialize_with`) through
            // `missing_field`, which an `Option` payload answers with `None`.
            let absent_is_none = in_request_view()
                && !extract_field_codec(&variant.attrs).deserialize
                && matches!(&variant.fields, syn::Fields::Unnamed(fields)
                    if fields.unnamed.len() == 1 && is_option_type(&fields.unnamed[0].ty));
            if !absent_is_none {
                required.push(content_string.clone());
            }
        } else if matches!(variant.fields, syn::Fields::Unit) {
            // serde reads a unit variant's content key when present — even
            // in an open container it is the content, never an ignored key —
            // and accepts only `null` (`UntaggedUnitVisitor`), so it is
            // listed, optional (serde never writes it).
            properties.insert(
                content_string.clone(),
                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Null))),
            );
        }

        // serde's adjacently tagged visitor under `deny_unknown_fields`
        // rejects every key but the tag and the content.
        let variant_schema = Schema {
            description: variant_description,
            properties: Some(properties),
            required: Some(required),
            additional_properties: renames
                .deny_unknown_fields
                .then_some(AdditionalProperties::Bool(false)),
            ..Schema::object()
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        one_of: non_empty(one_of_schemas),
        discriminator: Some(Discriminator {
            property_name: tag_string,
            mapping: None,
        }),
        ..Default::default()
    }
}

/// Parse untagged enum: variant data only, no tag.
///
/// Uses `anyOf` (not `oneOf`): serde picks the FIRST variant that
/// deserializes, so overlapping variants (e.g. `Int(i64)` / `Float(f64)`, or
/// a struct variant whose fields are a subset of another) are legal — `oneOf`
/// would make such values fail validation for matching more than one branch.
pub(super) fn parse_untagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    renames: EnumRenames<'_>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut any_of_schemas = Vec::with_capacity(enum_item.variants.len());

    for variant in emitted_variants(enum_item) {
        let variant_description = extract_doc_comment(&variant.attrs);
        let variant_schema = match (variant_codec_schema(variant), &variant.fields) {
            (_, syn::Fields::Unit) => {
                // Unit variant in untagged enum: null
                Schema {
                    description: variant_description,
                    schema_type: Some(SchemaType::Null),
                    ..Default::default()
                }
            }
            (Some(codec), _) => {
                let mut schema = with_description(codec, None);
                schema.description = variant_description.or(schema.description);
                schema
            }
            (None, syn::Fields::Unnamed(fields_unnamed)) if fields_unnamed.unnamed.len() == 1 => {
                // Single field tuple variant - just the inner type; a variant
                // doc comment overrides the inner schema's own description.
                let inner = newtype_field_schema(
                    &fields_unnamed.unnamed[0],
                    known_schemas,
                    struct_definitions,
                );
                // A bare `$ref` needs no `allOf` wrapper unless a variant doc
                // comment has to sit beside it.
                if variant_description.is_none() && matches!(inner, SchemaRef::Ref(_)) {
                    any_of_schemas.push(inner);
                    continue;
                }
                let mut schema = with_description(inner, None);
                schema.description = variant_description.or(schema.description);
                schema
            }
            (None, syn::Fields::Unnamed(fields_unnamed)) => Schema {
                description: variant_description,
                ..tuple_fields_schema(fields_unnamed, false, known_schemas, struct_definitions)
            },
            (None, syn::Fields::Named(fields_named)) => build_struct_variant_fields(
                fields_named,
                renames.fields,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(variant_description, renames.deny_unknown_fields),
        };

        any_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        any_of: non_empty(any_of_schemas),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_closed;

#[cfg(test)]
mod tests_fidelity;
