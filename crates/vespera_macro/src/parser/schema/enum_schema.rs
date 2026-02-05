//! Enum to JSON Schema conversion for OpenAPI generation.
//!
//! This module handles the conversion of Rust enums (as parsed by syn)
//! into OpenAPI-compatible JSON Schema definitions using the `oneOf` pattern.

use std::collections::{BTreeMap, HashMap};

use syn::Type;
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::{
    serde_attrs::{
        extract_doc_comment, extract_field_rename, extract_rename_all, rename_field,
        strip_raw_prefix,
    },
    type_schema::parse_type_to_schema_ref,
};

/// Parses a Rust enum into an OpenAPI Schema.
///
/// For simple enums (all unit variants), produces a string schema with enum values.
/// For enums with data, produces a schema with oneOf variants.
///
/// Handles serde attributes:
/// - `rename_all`: Applies case conversion to variant names
/// - `rename`: Individual variant rename
/// - Doc comments: Extracted as descriptions
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

    // Check if all variants are unit variants
    let all_unit = enum_item
        .variants
        .iter()
        .all(|v| matches!(v.fields, syn::Fields::Unit));

    if all_unit {
        // Simple enum with string values
        let mut enum_values = Vec::new();

        for variant in &enum_item.variants {
            let variant_name = strip_raw_prefix(&variant.ident.to_string()).to_string();

            // Check for variant-level rename attribute first (takes precedence)
            let enum_value = if let Some(renamed) = extract_field_rename(&variant.attrs) {
                renamed
            } else {
                // Apply rename_all transformation if present
                rename_field(&variant_name, rename_all.as_deref())
            };

            enum_values.push(serde_json::Value::String(enum_value));
        }

        Schema {
            schema_type: Some(SchemaType::String),
            description: enum_description,
            r#enum: if enum_values.is_empty() {
                None
            } else {
                Some(enum_values)
            },
            ..Schema::string()
        }
    } else {
        // Enum with data - use oneOf
        let mut one_of_schemas = Vec::new();

        for variant in &enum_item.variants {
            let variant_name = strip_raw_prefix(&variant.ident.to_string()).to_string();

            // Check for variant-level rename attribute first (takes precedence)
            let variant_key = if let Some(renamed) = extract_field_rename(&variant.attrs) {
                renamed
            } else {
                // Apply rename_all transformation if present
                rename_field(&variant_name, rename_all.as_deref())
            };

            // Extract variant-level doc comment
            let variant_description = extract_doc_comment(&variant.attrs);

            let variant_schema = match &variant.fields {
                syn::Fields::Unit => {
                    // Unit variant: {"const": "VariantName"}
                    Schema {
                        description: variant_description,
                        r#enum: Some(vec![serde_json::Value::String(variant_key)]),
                        ..Schema::string()
                    }
                }
                syn::Fields::Unnamed(fields_unnamed) => {
                    // Tuple variant: {"VariantName": <inner_type>}
                    // For single field: {"VariantName": <type>}
                    // For multiple fields: {"VariantName": [<type1>, <type2>, ...]}
                    if fields_unnamed.unnamed.len() == 1 {
                        // Single field tuple variant
                        let inner_type = &fields_unnamed.unnamed[0].ty;
                        let inner_schema =
                            parse_type_to_schema_ref(inner_type, known_schemas, struct_definitions);

                        let mut properties = BTreeMap::new();
                        properties.insert(variant_key.clone(), inner_schema);

                        Schema {
                            description: variant_description.clone(),
                            properties: Some(properties),
                            required: Some(vec![variant_key]),
                            ..Schema::object()
                        }
                    } else {
                        // Multiple fields tuple variant - serialize as array
                        // serde serializes tuple variants as: {"VariantName": [value1, value2, ...]}
                        // For OpenAPI 3.1, we use prefixItems to represent tuple arrays
                        let mut tuple_item_schemas = Vec::new();
                        for field in &fields_unnamed.unnamed {
                            let field_schema = parse_type_to_schema_ref(
                                &field.ty,
                                known_schemas,
                                struct_definitions,
                            );
                            tuple_item_schemas.push(field_schema);
                        }

                        let tuple_len = tuple_item_schemas.len();

                        // Create array schema with prefixItems for tuple arrays (OpenAPI 3.1)
                        let array_schema = Schema {
                            prefix_items: Some(tuple_item_schemas),
                            min_items: Some(tuple_len),
                            max_items: Some(tuple_len),
                            items: None, // Do not use prefixItems and items together
                            ..Schema::new(SchemaType::Array)
                        };

                        let mut properties = BTreeMap::new();
                        properties.insert(
                            variant_key.clone(),
                            SchemaRef::Inline(Box::new(array_schema)),
                        );

                        Schema {
                            description: variant_description.clone(),
                            properties: Some(properties),
                            required: Some(vec![variant_key]),
                            ..Schema::object()
                        }
                    }
                }
                syn::Fields::Named(fields_named) => {
                    // Struct variant: {"VariantName": {field1: type1, field2: type2, ...}}
                    let mut variant_properties = BTreeMap::new();
                    let mut variant_required = Vec::new();
                    let variant_rename_all = extract_rename_all(&variant.attrs);

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
                                variant_rename_all.as_deref().or(rename_all.as_deref()),
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

                    // Wrap struct variant in an object with the variant name as key
                    let inner_struct_schema = Schema {
                        properties: if variant_properties.is_empty() {
                            None
                        } else {
                            Some(variant_properties)
                        },
                        required: if variant_required.is_empty() {
                            None
                        } else {
                            Some(variant_required)
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
            schema_type: None, // oneOf doesn't have a single type
            description: enum_description,
            one_of: if one_of_schemas.is_empty() {
                None
            } else {
                Some(one_of_schemas)
            },
            ..Schema::new(SchemaType::Object)
        }
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
}
