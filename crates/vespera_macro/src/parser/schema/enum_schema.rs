//! Enum to JSON Schema conversion for OpenAPI generation.
//!
//! This module handles the conversion of Rust enums (as parsed by syn)
//! into OpenAPI-compatible JSON Schema definitions.
//!
//! ## Supported Serde Enum Representations
//!
//! Vespera supports all four serde enum representations:
//!
//! 1. **Externally Tagged** (default): `{"VariantName": {...}}`
//! 2. **Internally Tagged** (`#[serde(tag = "type")]`): `{"type": "VariantName", ...fields...}`
//! 3. **Adjacently Tagged** (`#[serde(tag = "type", content = "data")]`): `{"type": "VariantName", "data": {...}}`
//! 4. **Untagged** (`#[serde(untagged)]`): `{...fields...}` (no tag)
//!
//! Each representation maps to a different OpenAPI schema pattern using `oneOf` and optionally `discriminator`.

use std::collections::{BTreeMap, HashMap};

use syn::Type;
use vespera_core::schema::{Discriminator, Schema, SchemaRef, SchemaType};

use super::{
    serde_attrs::{
        SerdeEnumRepr, extract_doc_comment, extract_enum_repr, extract_field_rename,
        extract_rename_all, rename_field, strip_raw_prefix,
    },
    type_schema::parse_type_to_schema_ref,
};

/// Parses a Rust enum into an OpenAPI Schema.
///
/// Supports all four serde enum representations:
/// - Externally tagged (default): `{"VariantName": {...}}`
/// - Internally tagged (`#[serde(tag = "type")]`): `{"type": "VariantName", ...fields...}`
/// - Adjacently tagged (`#[serde(tag = "type", content = "data")]`): `{"type": "VariantName", "data": {...}}`
/// - Untagged (`#[serde(untagged)]`): `{...fields...}` (no tag)
///
/// # Arguments
/// * `enum_item` - The parsed enum from syn
/// * `known_schemas` - Map of known schema names for reference resolution
/// * `struct_definitions` - Map of struct names to their source code (for generics)
pub fn parse_enum_to_schema(
    enum_item: &syn::ItemEnum,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    // Extract enum-level doc comment for schema description
    let enum_description = extract_doc_comment(&enum_item.attrs);

    // Extract rename_all attribute from enum
    let rename_all = extract_rename_all(&enum_item.attrs);

    // Detect the serde enum representation
    let repr = extract_enum_repr(&enum_item.attrs);

    // Check if all variants are unit variants
    let all_unit = enum_item
        .variants
        .iter()
        .all(|v| matches!(v.fields, syn::Fields::Unit));

    // For simple enums (all unit variants) with externally tagged representation (default),
    // they serialize to just the variant name as a string.
    // However, internally/adjacently tagged enums serialize unit variants as objects with tag.
    if all_unit && matches!(repr, SerdeEnumRepr::ExternallyTagged) {
        return parse_unit_enum_to_schema(enum_item, enum_description, rename_all.as_deref());
    }

    match repr {
        SerdeEnumRepr::ExternallyTagged => parse_externally_tagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::InternallyTagged { tag } => parse_internally_tagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            &tag,
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::AdjacentlyTagged { tag, content } => parse_adjacently_tagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            &tag,
            &content,
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::Untagged => parse_untagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            known_schemas,
            struct_definitions,
        ),
    }
}

/// Parse a simple enum (all unit variants) to a string schema with enum values.
fn parse_unit_enum_to_schema(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
) -> Schema {
    let mut enum_values = Vec::new();

    for variant in &enum_item.variants {
        let variant_name = strip_raw_prefix(&variant.ident.to_string()).to_string();

        // Check for variant-level rename attribute first (takes precedence)
        let enum_value = if let Some(renamed) = extract_field_rename(&variant.attrs) {
            renamed
        } else {
            // Apply rename_all transformation if present
            rename_field(&variant_name, rename_all)
        };

        enum_values.push(serde_json::Value::String(enum_value));
    }

    Schema {
        schema_type: Some(SchemaType::String),
        description,
        r#enum: if enum_values.is_empty() {
            None
        } else {
            Some(enum_values)
        },
        ..Schema::string()
    }
}

/// Get the variant key (name after rename transformations)
fn get_variant_key(variant: &syn::Variant, rename_all: Option<&str>) -> String {
    let variant_name = strip_raw_prefix(&variant.ident.to_string()).to_string();

    if let Some(renamed) = extract_field_rename(&variant.attrs) {
        renamed
    } else {
        rename_field(&variant_name, rename_all)
    }
}

/// Build properties for a struct variant's fields
fn build_struct_variant_properties(
    fields_named: &syn::FieldsNamed,
    enum_rename_all: Option<&str>,
    variant_attrs: &[syn::Attribute],
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> (BTreeMap<String, SchemaRef>, Vec<String>) {
    let mut variant_properties = BTreeMap::new();
    let mut variant_required = Vec::new();
    let variant_rename_all = extract_rename_all(variant_attrs);

    for field in &fields_named.named {
        let rust_field_name = field
            .ident
            .as_ref()
            .map(|i| strip_raw_prefix(&i.to_string()).to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check for field-level rename attribute first (takes precedence)
        let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
            renamed
        } else {
            // Apply rename_all transformation if present
            rename_field(
                &rust_field_name,
                variant_rename_all.as_deref().or(enum_rename_all),
            )
        };

        let field_type = &field.ty;
        let mut schema_ref =
            parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

        // Extract doc comment from field and set as description
        if let Some(doc) = extract_doc_comment(&field.attrs) {
            match &mut schema_ref {
                SchemaRef::Inline(schema) => {
                    schema.description = Some(doc);
                }
                SchemaRef::Ref(_) => {
                    let ref_schema = std::mem::replace(
                        &mut schema_ref,
                        SchemaRef::Inline(Box::new(Schema::object())),
                    );
                    if let SchemaRef::Ref(reference) = ref_schema {
                        schema_ref = SchemaRef::Inline(Box::new(Schema {
                            description: Some(doc),
                            all_of: Some(vec![SchemaRef::Ref(reference)]),
                            ..Default::default()
                        }));
                    }
                }
            }
        }

        variant_properties.insert(field_name.clone(), schema_ref);

        // Check if field is Option<T>
        let is_optional = matches!(
            field_type,
            Type::Path(type_path)
                if type_path
                    .path
                    .segments
                    .first()
                    .map(|s| s.ident == "Option")
                    .unwrap_or(false)
        );

        if !is_optional {
            variant_required.push(field_name);
        }
    }

    (variant_properties, variant_required)
}

/// Build a schema for a variant's data (tuple or struct fields)
fn build_variant_data_schema(
    variant: &syn::Variant,
    enum_rename_all: Option<&str>,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<SchemaRef> {
    match &variant.fields {
        syn::Fields::Unit => None,
        syn::Fields::Unnamed(fields_unnamed) => {
            if fields_unnamed.unnamed.len() == 1 {
                // Single field tuple variant - just the inner type
                let inner_type = &fields_unnamed.unnamed[0].ty;
                Some(parse_type_to_schema_ref(
                    inner_type,
                    known_schemas,
                    struct_definitions,
                ))
            } else {
                // Multiple fields tuple variant - array with prefixItems
                let mut tuple_item_schemas = Vec::new();
                for field in &fields_unnamed.unnamed {
                    let field_schema =
                        parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions);
                    tuple_item_schemas.push(field_schema);
                }

                let tuple_len = tuple_item_schemas.len();
                Some(SchemaRef::Inline(Box::new(Schema {
                    prefix_items: Some(tuple_item_schemas),
                    min_items: Some(tuple_len),
                    max_items: Some(tuple_len),
                    items: None,
                    ..Schema::new(SchemaType::Array)
                })))
            }
        }
        syn::Fields::Named(fields_named) => {
            let (properties, required) = build_struct_variant_properties(
                fields_named,
                enum_rename_all,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            );

            Some(SchemaRef::Inline(Box::new(Schema {
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
            })))
        }
    }
}

/// Parse externally tagged enum: `{"VariantName": {...}}`
/// This is serde's default representation.
fn parse_externally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut one_of_schemas = Vec::new();

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
                    let mut tuple_item_schemas = Vec::new();
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
/// Uses OpenAPI discriminator for the tag field.
/// Note: serde only allows struct and unit variants for internally tagged enums.
fn parse_internally_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    tag: &str,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut one_of_schemas = Vec::new();
    let mut discriminator_mapping = BTreeMap::new();

    for variant in &enum_item.variants {
        let variant_key = get_variant_key(variant, rename_all);
        let variant_description = extract_doc_comment(&variant.attrs);

        let variant_schema = match &variant.fields {
            syn::Fields::Unit => {
                // Unit variant: {"tag": "VariantName"}
                let mut properties = BTreeMap::new();
                properties.insert(
                    tag.to_string(),
                    SchemaRef::Inline(Box::new(Schema {
                        r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                        ..Schema::string()
                    })),
                );

                Schema {
                    description: variant_description,
                    properties: Some(properties),
                    required: Some(vec![tag.to_string()]),
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
                    tag.to_string(),
                    SchemaRef::Inline(Box::new(Schema {
                        r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                        ..Schema::string()
                    })),
                );
                required.insert(0, tag.to_string());

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

        // Add to discriminator mapping (variant_key -> inline schema reference)
        // For inline schemas, we use #variant_key as a pseudo-reference
        discriminator_mapping.insert(variant_key.clone(), format!("#variant_{}", variant_key));

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
            property_name: tag.to_string(),
            mapping: None, // Mapping not needed for inline schemas
        }),
        ..Default::default()
    }
}

/// Parse adjacently tagged enum: `{"tag": "VariantName", "content": {...}}`
/// Uses OpenAPI discriminator for the tag field.
fn parse_adjacently_tagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    tag: &str,
    content: &str,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut one_of_schemas = Vec::new();

    for variant in &enum_item.variants {
        let variant_key = get_variant_key(variant, rename_all);
        let variant_description = extract_doc_comment(&variant.attrs);

        let mut properties = BTreeMap::new();
        let mut required = vec![tag.to_string()];

        // Add the tag field
        properties.insert(
            tag.to_string(),
            SchemaRef::Inline(Box::new(Schema {
                r#enum: Some(vec![serde_json::Value::String(variant_key.clone())]),
                ..Schema::string()
            })),
        );

        // Add the content field if variant has data
        if let Some(data_schema) =
            build_variant_data_schema(variant, rename_all, known_schemas, struct_definitions)
        {
            properties.insert(content.to_string(), data_schema);
            required.push(content.to_string());
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
            property_name: tag.to_string(),
            mapping: None,
        }),
        ..Default::default()
    }
}

/// Parse untagged enum: variant data only, no tag.
/// Uses oneOf without discriminator - validation relies on schema structure matching.
fn parse_untagged_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut one_of_schemas = Vec::new();

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
                    let mut tuple_item_schemas = Vec::new();
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
mod tests {
    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(
        r#"
        #[serde(rename_all = "kebab-case")]
        enum Status {
            #[serde(rename = "ok-status")]
            Ok,
            ErrorCode,
        }
        "#,
        SchemaType::String,
        vec!["ok-status", "error-code"],
        "status"
    )]
    #[case(
        r#"
        enum Simple {
            First,
            Second,
        }
        "#,
        SchemaType::String,
        vec!["First", "Second"],
        "simple"
    )]
    #[case(
        r#"
        #[serde(rename_all = "snake_case")]
        enum Simple {
            FirstItem,
            SecondItem,
        }
        "#,
        SchemaType::String,
        vec!["first_item", "second_item"],
        "simple_snake"
    )]
    fn test_parse_enum_to_schema_unit_variants(
        #[case] enum_src: &str,
        #[case] expected_type: SchemaType,
        #[case] expected_enum: Vec<&str>,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        assert_eq!(schema.schema_type, Some(expected_type));
        let got = schema
            .clone()
            .r#enum
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(got, expected_enum);
        with_settings!({ snapshot_suffix => format!("unit_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r#"
        enum Event {
            Data(String),
        }
        "#,
        1,
        Some(SchemaType::String),
        0, // single-field tuple variant stored as object with inline schema
        "tuple_single"
    )]
    #[case(
        r#"
        enum Pair {
            Values(i32, String),
        }
        "#,
        1,
        Some(SchemaType::Array),
        2, // tuple array prefix_items length
        "tuple_multi"
    )]
    #[case(
        r#"
        enum Msg {
            Detail { id: i32, note: Option<String> },
        }
        "#,
        1,
        Some(SchemaType::Object),
        0, // not an array; ignore prefix_items length
        "named_object"
    )]
    fn test_parse_enum_to_schema_tuple_and_named_variants(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_inner_type: Option<SchemaType>,
        #[case] expected_prefix_items_len: usize,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.clone().one_of.expect("one_of missing");
        assert_eq!(one_of.len(), expected_one_of_len);

        if let Some(inner_expected) = expected_inner_type.clone() {
            if let SchemaRef::Inline(obj) = &one_of[0] {
                let props = obj.properties.as_ref().expect("props missing");
                // take first property value
                let inner_schema = props.values().next().expect("no property value");
                match inner_expected {
                    SchemaType::Array => {
                        if let SchemaRef::Inline(array_schema) = inner_schema {
                            assert_eq!(array_schema.schema_type, Some(SchemaType::Array));
                            if expected_prefix_items_len > 0 {
                                assert_eq!(
                                    array_schema.prefix_items.as_ref().unwrap().len(),
                                    expected_prefix_items_len
                                );
                            }
                        } else {
                            panic!("Expected inline array schema");
                        }
                    }
                    SchemaType::Object => {
                        if let SchemaRef::Inline(inner_obj) = inner_schema {
                            assert_eq!(inner_obj.schema_type, Some(SchemaType::Object));
                            let inner_props = inner_obj.properties.as_ref().unwrap();
                            assert!(inner_props.contains_key("id"));
                            assert!(inner_props.contains_key("note"));
                            assert!(
                                inner_obj
                                    .required
                                    .as_ref()
                                    .unwrap()
                                    .contains(&"id".to_string())
                            );
                        } else {
                            panic!("Expected inline object schema");
                        }
                    }
                    _ => {}
                }
            } else {
                panic!("Expected inline schema in one_of");
            }
        }

        with_settings!({ snapshot_suffix => format!("tuple_named_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r#"
        enum Mixed {
            Ready,
            Data(String),
        }
        "#,
        2,
        SchemaType::String,
        "Ready"
    )]
    fn test_parse_enum_to_schema_mixed_unit_variant(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_unit_type: SchemaType,
        #[case] expected_unit_value: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing for mixed enum");
        assert_eq!(one_of.len(), expected_one_of_len);

        let unit_schema = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema for unit variant"),
        };
        assert_eq!(unit_schema.schema_type, Some(expected_unit_type));
        let unit_enum = unit_schema.r#enum.as_ref().expect("enum values missing");
        assert_eq!(unit_enum[0].as_str().unwrap(), expected_unit_value);
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_for_data_variant() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "kebab-case")]
            enum Payload {
                DataItem(String),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("data-item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_uses_enum_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Event {
                Detail { UserId: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let inner = match props.get("detail").expect("variant key missing") {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline inner schema"),
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        assert!(inner_props.contains_key("user_id"));
        assert!(!inner_props.contains_key("UserId"));
    }

    #[test]
    fn test_parse_enum_to_schema_variant_rename_overrides_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename = "Explicit")]
                DataItem(i32),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("Explicit"));
        assert!(!props.contains_key("data_item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_rename_overrides_variant_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename_all = "kebab-case")]
                Detail { #[serde(rename = "ID")] user_id: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let inner = match props
            .get("detail")
            .or_else(|| props.get("Detail"))
            .expect("variant key missing")
        {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline inner schema"),
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        assert!(inner_props.contains_key("ID")); // field-level rename wins
        assert!(!inner_props.contains_key("user-id")); // variant rename_all ignored for this field
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_with_other_attrs_unit() {
        // Test rename_all combined with other serde attributes for unit variants
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "kebab-case", default)]
            enum Status {
                ActiveUser,
                InactiveUser,
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let enum_values = schema.r#enum.expect("enum values missing");
        assert_eq!(enum_values[0].as_str().unwrap(), "active-user");
        assert_eq!(enum_values[1].as_str().unwrap(), "inactive-user");
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_with_other_attrs_data() {
        // Test rename_all combined with other serde attributes for data variants
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            enum Event {
                UserCreated { user_name: String, created_at: i64 },
                UserDeleted(i32),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");

        // Check UserCreated variant key is camelCase
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("userCreated"));
        assert!(!props.contains_key("UserCreated"));
        assert!(!props.contains_key("user_created"));

        // Check UserDeleted variant key is camelCase
        let variant_obj2 = match &one_of[1] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props2 = variant_obj2
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props2.contains_key("userDeleted"));
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_not_first_attr() {
        // Test rename_all when it's not the first attribute
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(default, rename_all = "SCREAMING_SNAKE_CASE")]
            enum Priority {
                HighPriority,
                LowPriority,
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let enum_values = schema.r#enum.expect("enum values missing");
        assert_eq!(enum_values[0].as_str().unwrap(), "HIGH_PRIORITY");
        assert_eq!(enum_values[1].as_str().unwrap(), "LOW_PRIORITY");
    }

    // Test enum with empty variants (edge case)
    #[test]
    fn test_parse_enum_to_schema_empty_enum() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            enum Empty {}
        "#,
        )
        .unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        // Empty enum should have no enum values
        assert!(schema.r#enum.is_none() || schema.r#enum.as_ref().unwrap().is_empty());
    }

    // Test enum with all struct variants having empty properties
    #[test]
    fn test_parse_enum_to_schema_struct_variant_no_fields() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            enum Event {
                Empty {},
            }
        "#,
        )
        .unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        assert_eq!(one_of.len(), 1);
    }

    // Tests for enum with doc comments on variants
    #[test]
    fn test_parse_enum_to_schema_with_variant_descriptions() {
        let enum_src = r#"
            /// Enum description
            enum Status {
                /// Active variant
                Active,
                /// Inactive variant
                Inactive,
            }
        "#;
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        assert_eq!(schema.description, Some("Enum description".to_string()));
    }

    #[test]
    fn test_parse_enum_to_schema_data_variant_with_description() {
        let enum_src = r#"
            /// Data enum
            enum Event {
                /// Text event description
                Text(String),
                /// Number event description
                Number(i32),
            }
        "#;
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        assert_eq!(schema.description, Some("Data enum".to_string()));
        assert!(schema.one_of.is_some());
        let one_of = schema.one_of.unwrap();
        assert_eq!(one_of.len(), 2);
        // Check first variant has description
        if let SchemaRef::Inline(variant_schema) = &one_of[0] {
            assert_eq!(
                variant_schema.description,
                Some("Text event description".to_string())
            );
        }
    }

    #[test]
    fn test_parse_enum_to_schema_struct_variant_with_field_docs() {
        let enum_src = r#"
            enum Event {
                /// Record variant
                Record {
                    /// The value field
                    value: i32,
                    /// The name field
                    name: String,
                },
            }
        "#;
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        assert!(schema.one_of.is_some());
        let one_of = schema.one_of.unwrap();
        if let SchemaRef::Inline(variant_schema) = &one_of[0] {
            assert_eq!(
                variant_schema.description,
                Some("Record variant".to_string())
            );
        }
    }

    #[test]
    fn test_parse_enum_to_schema_variant_field_with_doc_comment_and_ref() {
        // Test that doc comment on field with SchemaRef::Ref wraps in allOf
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            enum Message {
                Data {
                    /// The user associated with this message
                    user: User,
                },
            }
        "#,
        )
        .unwrap();

        // Register User as a known schema to get SchemaRef::Ref
        let mut known_schemas = HashMap::new();
        known_schemas.insert("User".to_string(), "User".to_string());

        let schema = parse_enum_to_schema(&enum_item, &known_schemas, &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");

        // Get the Data variant schema
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let inner = match props.get("Data").expect("variant key missing") {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline inner schema"),
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");

        // The user field should have been wrapped in allOf with description
        let user_field = inner_props.get("user").expect("user field missing");
        match user_field {
            SchemaRef::Inline(schema) => {
                // Should have description from doc comment
                assert_eq!(
                    schema.description.as_deref(),
                    Some("The user associated with this message")
                );
                // Should have allOf with the original $ref
                let all_of = schema.all_of.as_ref().expect("allOf missing");
                assert_eq!(all_of.len(), 1);
                match &all_of[0] {
                    SchemaRef::Ref(reference) => {
                        assert_eq!(reference.ref_path, "#/components/schemas/User");
                    }
                    _ => panic!("Expected $ref in allOf"),
                }
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema with allOf, not direct $ref"),
        }
    }

    // Tests for serde enum representation support
    mod enum_repr_tests {
        use super::*;

        // Internally tagged enum tests
        #[test]
        fn test_internally_tagged_enum_unit_variants() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type")]
                enum Message {
                    Ping,
                    Pong,
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            // Should have discriminator
            let discriminator = schema
                .discriminator
                .as_ref()
                .expect("discriminator missing");
            assert_eq!(discriminator.property_name, "type");

            // Should have oneOf
            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Each variant should be an object with "type" property
            if let SchemaRef::Inline(ping) = &one_of[0] {
                let props = ping.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("type"));
                let required = ping.required.as_ref().expect("required missing");
                assert!(required.contains(&"type".to_string()));
            } else {
                panic!("Expected inline schema");
            }
        }

        #[test]
        fn test_internally_tagged_enum_struct_variants() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "kind")]
                enum Event {
                    Created { id: i32, name: String },
                    Updated { id: i32 },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            // Should have discriminator with custom tag name
            let discriminator = schema
                .discriminator
                .as_ref()
                .expect("discriminator missing");
            assert_eq!(discriminator.property_name, "kind");

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Created variant should have kind, id, and name
            if let SchemaRef::Inline(created) = &one_of[0] {
                let props = created.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("kind"));
                assert!(props.contains_key("id"));
                assert!(props.contains_key("name"));
            } else {
                panic!("Expected inline schema");
            }
        }

        #[test]
        fn test_internally_tagged_enum_with_rename_all() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type", rename_all = "snake_case")]
                enum Status {
                    ActiveUser,
                    InactiveUser,
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            let one_of = schema.one_of.expect("one_of missing");
            if let SchemaRef::Inline(active) = &one_of[0] {
                let props = active.properties.as_ref().expect("properties missing");
                if let SchemaRef::Inline(type_schema) = props.get("type").expect("type missing") {
                    let enum_vals = type_schema.r#enum.as_ref().expect("enum values missing");
                    assert_eq!(enum_vals[0].as_str().unwrap(), "active_user");
                }
            }
        }

        // Adjacently tagged enum tests
        #[test]
        fn test_adjacently_tagged_enum_basic() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type", content = "data")]
                enum Response {
                    Success { result: String },
                    Error { message: String },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            // Should have discriminator
            let discriminator = schema
                .discriminator
                .as_ref()
                .expect("discriminator missing");
            assert_eq!(discriminator.property_name, "type");

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Each variant should have "type" and "data" properties
            if let SchemaRef::Inline(success) = &one_of[0] {
                let props = success.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("type"));
                assert!(props.contains_key("data"));

                let required = success.required.as_ref().expect("required missing");
                assert!(required.contains(&"type".to_string()));
                assert!(required.contains(&"data".to_string()));
            } else {
                panic!("Expected inline schema");
            }
        }

        #[test]
        fn test_adjacently_tagged_enum_with_unit_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type", content = "payload")]
                enum Command {
                    Ping,
                    Message { text: String },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Ping (unit variant) should only have "type", no "payload"
            if let SchemaRef::Inline(ping) = &one_of[0] {
                let props = ping.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("type"));
                assert!(!props.contains_key("payload")); // Unit variant has no content

                let required = ping.required.as_ref().expect("required missing");
                assert_eq!(required.len(), 1); // Only "type" is required
                assert!(required.contains(&"type".to_string()));
            }

            // Message should have both "type" and "payload"
            if let SchemaRef::Inline(message) = &one_of[1] {
                let props = message.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("type"));
                assert!(props.contains_key("payload"));
            }
        }

        #[test]
        fn test_adjacently_tagged_enum_tuple_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "t", content = "c")]
                enum Value {
                    Int(i32),
                    Pair(i32, String),
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Int variant - content should be integer schema
            if let SchemaRef::Inline(int_variant) = &one_of[0] {
                let props = int_variant.properties.as_ref().expect("properties missing");
                let content = props.get("c").expect("content missing");
                if let SchemaRef::Inline(content_schema) = content {
                    assert_eq!(content_schema.schema_type, Some(SchemaType::Integer));
                }
            }

            // Pair variant - content should be array with prefixItems
            if let SchemaRef::Inline(pair_variant) = &one_of[1] {
                let props = pair_variant
                    .properties
                    .as_ref()
                    .expect("properties missing");
                let content = props.get("c").expect("content missing");
                if let SchemaRef::Inline(content_schema) = content {
                    assert_eq!(content_schema.schema_type, Some(SchemaType::Array));
                    assert!(content_schema.prefix_items.is_some());
                }
            }
        }

        // Untagged enum tests
        #[test]
        fn test_untagged_enum_basic() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum StringOrInt {
                    String(String),
                    Int(i32),
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            // Should NOT have discriminator
            assert!(schema.discriminator.is_none());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // First variant should be string schema directly (not wrapped in object)
            if let SchemaRef::Inline(string_variant) = &one_of[0] {
                assert_eq!(string_variant.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline schema");
            }

            // Second variant should be integer schema directly
            if let SchemaRef::Inline(int_variant) = &one_of[1] {
                assert_eq!(int_variant.schema_type, Some(SchemaType::Integer));
            } else {
                panic!("Expected inline schema");
            }
        }

        #[test]
        fn test_untagged_enum_struct_variants() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum Data {
                    User { name: String, age: i32 },
                    Product { title: String, price: f64 },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            assert!(schema.discriminator.is_none());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // User variant should be object with name and age (no wrapper)
            if let SchemaRef::Inline(user) = &one_of[0] {
                assert_eq!(user.schema_type, Some(SchemaType::Object));
                let props = user.properties.as_ref().expect("properties missing");
                assert!(props.contains_key("name"));
                assert!(props.contains_key("age"));
            }
        }

        #[test]
        fn test_untagged_enum_unit_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum MaybeValue {
                    Nothing,
                    Something(i32),
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Unit variant in untagged enum should be null
            if let SchemaRef::Inline(nothing) = &one_of[0] {
                assert_eq!(nothing.schema_type, Some(SchemaType::Null));
            }
        }

        // Snapshot tests for new representations
        #[test]
        fn test_internally_tagged_snapshot() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type")]
                enum Message {
                    Request { id: i32, method: String },
                    Response { id: i32, result: Option<String> },
                    Notification,
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
            with_settings!({ snapshot_suffix => "internally_tagged" }, {
                assert_debug_snapshot!(schema);
            });
        }

        #[test]
        fn test_adjacently_tagged_snapshot() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type", content = "data")]
                enum ApiResponse {
                    Success { items: Vec<String> },
                    Error { code: i32, message: String },
                    Empty,
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
            with_settings!({ snapshot_suffix => "adjacently_tagged" }, {
                assert_debug_snapshot!(schema);
            });
        }

        #[test]
        fn test_untagged_snapshot() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum Value {
                    Null,
                    Bool(bool),
                    Number(f64),
                    Text(String),
                    Object { key: String, value: String },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
            with_settings!({ snapshot_suffix => "untagged" }, {
                assert_debug_snapshot!(schema);
            });
        }

        // Edge case: Empty struct variant (empty properties/required)
        #[test]
        fn test_externally_tagged_empty_struct_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                enum Event {
                    /// Empty struct variant
                    Empty {},
                    Data { value: i32 },
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            let one_of = schema.clone().one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // Empty variant should have properties with Empty key pointing to object with no properties
            if let SchemaRef::Inline(empty_variant) = &one_of[0] {
                let props = empty_variant
                    .properties
                    .as_ref()
                    .expect("variant props missing");
                let inner = match props.get("Empty").expect("Empty key missing") {
                    SchemaRef::Inline(s) => s,
                    _ => panic!("Expected inline schema"),
                };
                // Empty struct should have properties: None and required: None
                assert!(inner.properties.is_none());
                assert!(inner.required.is_none());
            }

            with_settings!({ snapshot_suffix => "externally_tagged_empty_struct" }, {
                assert_debug_snapshot!(schema);
            });
        }

        // Edge case: Internally tagged enum with tuple variant
        #[test]
        fn test_internally_tagged_skips_tuple_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(tag = "type")]
                enum Message {
                    Text { content: String },
                    Number(i32),
                    Empty,
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            // Tuple variant `Number(i32)` should be skipped, only 2 variants should remain
            let one_of = schema.clone().one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2); // Text and Empty only

            // Verify discriminator is present
            let discriminator = schema
                .discriminator
                .as_ref()
                .expect("discriminator missing");
            assert_eq!(discriminator.property_name, "type");

            with_settings!({ snapshot_suffix => "internally_tagged_skip_tuple" }, {
                assert_debug_snapshot!(schema);
            });
        }

        // Edge case: Untagged enum with tuple variant referencing a known schema
        #[test]
        fn test_untagged_tuple_variant_with_known_schema_ref() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum Payload {
                    User(UserData),
                    Simple(String),
                }
                "#,
            )
            .unwrap();

            // Provide UserData as a known schema so it returns SchemaRef::Ref
            let mut known_schemas = HashMap::new();
            known_schemas.insert("UserData".to_string(), "UserData".to_string());

            let schema = parse_enum_to_schema(&enum_item, &known_schemas, &HashMap::new());

            assert!(schema.discriminator.is_none());

            let one_of = schema.one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 2);

            // First variant (UserData) should have all_of with a $ref since it's a known schema
            if let SchemaRef::Inline(user_variant) = &one_of[0] {
                // The schema should have all_of containing the reference
                let all_of = user_variant
                    .all_of
                    .as_ref()
                    .expect("all_of missing for known schema ref");
                assert_eq!(all_of.len(), 1);
                if let SchemaRef::Ref(reference) = &all_of[0] {
                    assert!(reference.ref_path.contains("UserData"));
                } else {
                    panic!("Expected SchemaRef::Ref inside all_of");
                }
            } else {
                panic!("Expected inline schema");
            }

            // Second variant (String) should be inline string schema directly
            if let SchemaRef::Inline(simple_variant) = &one_of[1] {
                assert_eq!(simple_variant.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline schema");
            }
        }

        // Edge case: Untagged enum with multi-field tuple variant
        #[test]
        fn test_untagged_multi_field_tuple_variant() {
            let enum_item: syn::ItemEnum = syn::parse_str(
                r#"
                #[serde(untagged)]
                enum Message {
                    Text(String),
                    Pair(i32, String),
                    Triple(i32, String, bool),
                }
                "#,
            )
            .unwrap();

            let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());

            assert!(schema.discriminator.is_none());

            let one_of = schema.clone().one_of.expect("one_of missing");
            assert_eq!(one_of.len(), 3);

            // Single-field tuple should be string schema directly
            if let SchemaRef::Inline(text_variant) = &one_of[0] {
                assert_eq!(text_variant.schema_type, Some(SchemaType::String));
            }

            // Multi-field tuple (Pair) should be array with prefixItems
            if let SchemaRef::Inline(pair_variant) = &one_of[1] {
                assert_eq!(pair_variant.schema_type, Some(SchemaType::Array));
                let prefix_items = pair_variant
                    .prefix_items
                    .as_ref()
                    .expect("prefix_items missing for Pair");
                assert_eq!(prefix_items.len(), 2);
                assert_eq!(pair_variant.min_items, Some(2));
                assert_eq!(pair_variant.max_items, Some(2));
            }

            // Multi-field tuple (Triple) should be array with 3 prefixItems
            if let SchemaRef::Inline(triple_variant) = &one_of[2] {
                assert_eq!(triple_variant.schema_type, Some(SchemaType::Array));
                let prefix_items = triple_variant
                    .prefix_items
                    .as_ref()
                    .expect("prefix_items missing for Triple");
                assert_eq!(prefix_items.len(), 3);
                assert_eq!(triple_variant.min_items, Some(3));
                assert_eq!(triple_variant.max_items, Some(3));
            }

            with_settings!({ snapshot_suffix => "untagged_multi_field_tuple" }, {
                assert_debug_snapshot!(schema);
            });
        }
    }
}
