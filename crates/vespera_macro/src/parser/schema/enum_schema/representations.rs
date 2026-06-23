use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Discriminator, Schema, SchemaRef, SchemaType};

use super::super::{serde_attrs::extract_doc_comment, type_schema::parse_type_to_schema_ref};
use super::{
    unit::get_variant_key,
    variant::{build_struct_variant_properties, build_variant_data_schema},
};

/// Parse externally tagged enum: `{"VariantName": {...}}`
/// This is serde's default representation.
pub(super) fn parse_externally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());

    for variant in &enum_item.variants {
        let variant_key = get_variant_key(variant, rename_all);
        let variant_description = extract_doc_comment(&variant.attrs);

        let variant_schema = match &variant.fields {
            syn::Fields::Unit => {
                // Unit variant in mixed enum: string with const value
                Schema {
                    description: variant_description,
                    r#enum: Some(vec![serde_json::Value::String(variant_key)]),
                    ..Schema::string()
                }
            }
            syn::Fields::Unnamed(fields_unnamed) => {
                // Tuple variant: {"VariantName": <data>}
                let data_schema = if fields_unnamed.unnamed.len() == 1 {
                    let inner_type = &fields_unnamed.unnamed[0].ty;
                    parse_type_to_schema_ref(inner_type, known_schemas, struct_definitions)
                } else {
                    // Multiple fields - array with prefixItems
                    let mut tuple_item_schemas = Vec::with_capacity(fields_unnamed.unnamed.len());
                    for field in &fields_unnamed.unnamed {
                        let field_schema =
                            parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions);
                        tuple_item_schemas.push(field_schema);
                    }
                    let tuple_len = tuple_item_schemas.len();
                    SchemaRef::Inline(Box::new(Schema {
                        prefix_items: Some(tuple_item_schemas),
                        min_items: Some(tuple_len),
                        max_items: Some(tuple_len),
                        items: None,
                        ..Schema::new(SchemaType::Array)
                    }))
                };

                let mut properties = BTreeMap::new();
                properties.insert(variant_key.clone(), data_schema);

                Schema {
                    description: variant_description,
                    properties: Some(properties),
                    required: Some(vec![variant_key]),
                    ..Schema::object()
                }
            }
            syn::Fields::Named(fields_named) => {
                // Struct variant: {"VariantName": {field1: type1, ...}}
                let (inner_properties, inner_required) = build_struct_variant_properties(
                    fields_named,
                    rename_all,
                    &variant.attrs,
                    known_schemas,
                    struct_definitions,
                );

                let inner_struct_schema = Schema {
                    properties: if inner_properties.is_empty() {
                        None
                    } else {
                        Some(inner_properties)
                    },
                    required: if inner_required.is_empty() {
                        None
                    } else {
                        Some(inner_required)
                    },
                    ..Schema::object()
                };

                let mut properties = BTreeMap::new();
                properties.insert(
                    variant_key.clone(),
                    SchemaRef::Inline(Box::new(inner_struct_schema)),
                );

                Schema {
                    description: variant_description,
                    properties: Some(properties),
                    required: Some(vec![variant_key]),
                    ..Schema::object()
                }
            }
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        one_of: if one_of_schemas.is_empty() {
            None
        } else {
            Some(one_of_schemas)
        },
        ..Schema::new(SchemaType::Object)
    }
}

/// Parse internally tagged enum: `{"tag": "VariantName", ...fields...}`
/// Uses `OpenAPI` discriminator for the tag field.
/// Note: serde only allows struct and unit variants for internally tagged enums.
pub(super) fn parse_internally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    tag: &str,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());

    let tag_string = tag.to_string();

    for variant in &enum_item.variants {
        let variant_key = get_variant_key(variant, rename_all);
        let variant_description = extract_doc_comment(&variant.attrs);

        let variant_schema = match &variant.fields {
            syn::Fields::Unit => {
                // Unit variant: {"tag": "VariantName"}
                let mut properties = BTreeMap::new();
                properties.insert(
                    tag_string.clone(),
                    SchemaRef::Inline(Box::new(Schema {
                        r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                        ..Schema::string()
                    })),
                );

                Schema {
                    description: variant_description,
                    properties: Some(properties),
                    required: Some(vec![tag_string.clone()]),
                    ..Schema::object()
                }
            }
            syn::Fields::Named(fields_named) => {
                // Struct variant: {"tag": "VariantName", field1: type1, ...}
                let (mut properties, mut required) = build_struct_variant_properties(
                    fields_named,
                    rename_all,
                    &variant.attrs,
                    known_schemas,
                    struct_definitions,
                );

                // Add the tag field
                properties.insert(
                    tag_string.clone(),
                    SchemaRef::Inline(Box::new(Schema {
                        r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                        ..Schema::string()
                    })),
                );
                required.insert(0, tag_string.clone());

                Schema {
                    description: variant_description,
                    properties: Some(properties),
                    required: Some(required),
                    ..Schema::object()
                }
            }
            syn::Fields::Unnamed(_) => {
                // Tuple/newtype variants are not supported with internally tagged enums in serde
                // Generate a warning schema or skip
                continue;
            }
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        one_of: if one_of_schemas.is_empty() {
            None
        } else {
            Some(one_of_schemas)
        },
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
    rename_all: Option<&str>,
    tag: &str,
    content: &str,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());

    let tag_string = tag.to_string();
    let content_string = content.to_string();

    for variant in &enum_item.variants {
        let variant_key = get_variant_key(variant, rename_all);
        let variant_description = extract_doc_comment(&variant.attrs);

        let mut properties = BTreeMap::new();
        let mut required = vec![tag_string.clone()];

        // Add the tag field
        properties.insert(
            tag_string.clone(),
            SchemaRef::Inline(Box::new(Schema {
                r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                ..Schema::string()
            })),
        );

        // Add the content field if variant has data
        if let Some(data_schema) =
            build_variant_data_schema(variant, rename_all, known_schemas, struct_definitions)
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
        one_of: if one_of_schemas.is_empty() {
            None
        } else {
            Some(one_of_schemas)
        },
        discriminator: Some(Discriminator {
            property_name: tag_string,
            mapping: None,
        }),
        ..Default::default()
    }
}

/// Parse untagged enum: variant data only, no tag.
/// Uses oneOf without discriminator - validation relies on schema structure matching.
pub(super) fn parse_untagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut one_of_schemas = Vec::with_capacity(enum_item.variants.len());

    for variant in &enum_item.variants {
        let variant_description = extract_doc_comment(&variant.attrs);

        let variant_schema = match &variant.fields {
            syn::Fields::Unit => {
                // Unit variant in untagged enum: null
                Schema {
                    description: variant_description,
                    schema_type: Some(SchemaType::Null),
                    ..Default::default()
                }
            }
            syn::Fields::Unnamed(fields_unnamed) => {
                if fields_unnamed.unnamed.len() == 1 {
                    // Single field tuple variant - just the inner type
                    let inner_type = &fields_unnamed.unnamed[0].ty;
                    let mut schema = match parse_type_to_schema_ref(
                        inner_type,
                        known_schemas,
                        struct_definitions,
                    ) {
                        SchemaRef::Inline(s) => *s,
                        SchemaRef::Ref(r) => Schema {
                            all_of: Some(vec![SchemaRef::Ref(r)]),
                            ..Default::default()
                        },
                    };
                    schema.description = variant_description.or(schema.description);
                    schema
                } else {
                    // Multiple fields - array with prefixItems
                    let mut tuple_item_schemas = Vec::with_capacity(fields_unnamed.unnamed.len());
                    for field in &fields_unnamed.unnamed {
                        let field_schema =
                            parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions);
                        tuple_item_schemas.push(field_schema);
                    }
                    let tuple_len = tuple_item_schemas.len();
                    Schema {
                        description: variant_description,
                        prefix_items: Some(tuple_item_schemas),
                        min_items: Some(tuple_len),
                        max_items: Some(tuple_len),
                        items: None,
                        ..Schema::new(SchemaType::Array)
                    }
                }
            }
            syn::Fields::Named(fields_named) => {
                // Struct variant - just the object with fields
                let (properties, required) = build_struct_variant_properties(
                    fields_named,
                    rename_all,
                    &variant.attrs,
                    known_schemas,
                    struct_definitions,
                );

                Schema {
                    description: variant_description,
                    properties: if properties.is_empty() {
                        None
                    } else {
                        Some(properties)
                    },
                    required: if required.is_empty() {
                        None
                    } else {
                        Some(required)
                    },
                    ..Schema::object()
                }
            }
        };

        one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
    }

    Schema {
        schema_type: None,
        description,
        one_of: if one_of_schemas.is_empty() {
            None
        } else {
            Some(one_of_schemas)
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
