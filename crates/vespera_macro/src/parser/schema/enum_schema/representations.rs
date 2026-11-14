use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{AdditionalProperties, Discriminator, Schema, SchemaRef, SchemaType};

use super::super::{
    request_view::in_request_view,
    serde_attrs::extract_doc_comment,
    struct_schema::{resolve_type, tuple_fields_schema, with_description},
};
use super::{
    EnumRenames,
    unit::{other_tag_schema, variant_keys},
    variant::{
        build_struct_variant_fields, build_variant_data_schema, is_other_variant,
        is_variant_omitted, newtype_field_schema, variant_codec_schema,
    },
};

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
            build_variant_data_schema(variant, renames.fields, known_schemas, struct_definitions)
        {
            // Tuple / struct variant: {"VariantName": <data>}
            for key in keys {
                one_of_schemas.push(SchemaRef::Inline(Box::new(Schema {
                    description: variant_description.clone(),
                    properties: Some(BTreeMap::from([(key.clone(), data_schema.clone())])),
                    required: Some(vec![key]),
                    ..Schema::object()
                })));
            }
        } else {
            // Unit variant in mixed enum: string with const value
            one_of_schemas.push(SchemaRef::Inline(Box::new(Schema {
                description: variant_description,
                ..tag_value_schema(keys)
            })));
        }
    }

    Schema {
        schema_type: None,
        description,
        one_of: non_empty(one_of_schemas),
        ..Schema::new(SchemaType::Object)
    }
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
/// payload (`()`, a unit struct) serializes as just the tag. Non-object
/// inner types are rejected at `#[derive(Schema)]` time (see
/// `validate_enum_for_schema`).
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
                fields.into_object_schema(variant_description, false)
            }
            (None, syn::Fields::Unnamed(fields_unnamed)) if fields_unnamed.unnamed.len() == 1 => {
                let field = &fields_unnamed.unnamed[0];
                let inner = newtype_field_schema(field, known_schemas, struct_definitions);
                // A `$ref` payload is resolved from its Rust type (a
                // request-view `$ref` names the `Input` component, so the
                // `$ref` itself cannot be looked up).
                let payload = match &inner {
                    SchemaRef::Inline(schema) => Some((**schema).clone()),
                    SchemaRef::Ref(_) => resolve_type(&field.ty, known_schemas, struct_definitions),
                };
                match payload.as_ref().and_then(|payload| payload.schema_type) {
                    // Newtype of `()` / a unit struct serializes as just the tag.
                    Some(SchemaType::Null) => tag_object(variant_description),
                    // A map-like (or referenced struct) inner value carries the
                    // tag: a closed payload object as one of its own properties.
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
                    // A `$ref` to a non-object component is reported once
                    // every component is known (see `validate_enum_for_schema`).
                    Some(_) if matches!(inner, SchemaRef::Ref(_)) => {
                        tag_and(inner, variant_description)
                    }
                    // Newtype of a non-map value cannot carry the tag in serde;
                    // rejected at derive time, skipped here defensively.
                    Some(_) => continue,
                }
            }
            // Multi-field tuple variants are a serde compile error here.
            (None, syn::Fields::Unnamed(_)) => continue,
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        one_of: non_empty(one_of_schemas),
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
            build_variant_data_schema(variant, renames.fields, known_schemas, struct_definitions)
        {
            properties.insert(content_string.clone(), data_schema);
            required.push(content_string.clone());
        }

        let variant_schema = Schema {
            description: variant_description,
            properties: Some(properties),
            required: Some(required),
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
                ..tuple_fields_schema(fields_unnamed, known_schemas, struct_definitions)
            },
            (None, syn::Fields::Named(fields_named)) => build_struct_variant_fields(
                fields_named,
                renames.fields,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(variant_description, false),
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
