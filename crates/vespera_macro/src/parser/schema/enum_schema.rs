//! Enum to JSON Schema conversion for OpenAPI generation.

use std::collections::{HashMap, HashSet};

use vespera_core::schema::Schema;

use super::serde_attrs::{
    SerdeEnumRepr, extract_doc_comment, extract_enum_repr, extract_rename_all,
};

mod representations;
mod unit;
mod variant;

/// Parses a Rust enum into an OpenAPI Schema.
pub fn parse_enum_to_schema(
    enum_item: &syn::ItemEnum,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let enum_description = extract_doc_comment(&enum_item.attrs);
    let rename_all = extract_rename_all(&enum_item.attrs);
    let repr = extract_enum_repr(&enum_item.attrs);
    let all_unit = enum_item
        .variants
        .iter()
        .all(|v| matches!(v.fields, syn::Fields::Unit));

    if all_unit && matches!(repr, SerdeEnumRepr::ExternallyTagged) {
        return unit::parse_unit_enum_to_schema(enum_item, enum_description, rename_all.as_deref());
    }

    match repr {
        SerdeEnumRepr::ExternallyTagged => representations::parse_externally_tagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::InternallyTagged { tag } => representations::parse_internally_tagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            &tag,
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::AdjacentlyTagged { tag, content } => {
            representations::parse_adjacently_tagged_enum(
                enum_item,
                enum_description,
                rename_all.as_deref(),
                &tag,
                &content,
                known_schemas,
                struct_definitions,
            )
        }
        SerdeEnumRepr::Untagged => representations::parse_untagged_enum(
            enum_item,
            enum_description,
            rename_all.as_deref(),
            known_schemas,
            struct_definitions,
        ),
    }
}

#[cfg(test)]
mod tests {
    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;

    use super::*;
    use vespera_core::schema::{SchemaRef, SchemaType};

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
        r"
        enum Simple {
            First,
            Second,
        }
        ",
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
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        assert_eq!(schema.schema_type, Some(expected_type));
        let got = schema
            .clone()
            .r#enum
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(got, expected_enum);
        with_settings!({ snapshot_path => "snapshots", snapshot_suffix => format!("unit_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r"
        enum Event {
            Data(String),
        }
        ",
        1,
        Some(SchemaType::String),
        0, // single-field tuple variant stored as object with inline schema
        "tuple_single"
    )]
    #[case(
        r"
        enum Pair {
            Values(i32, String),
        }
        ",
        1,
        Some(SchemaType::Array),
        2, // tuple array prefix_items length
        "tuple_multi"
    )]
    #[case(
        r"
        enum Msg {
            Detail { id: i32, note: Option<String> },
        }
        ",
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
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.clone().one_of.expect("one_of missing");
        assert_eq!(one_of.len(), expected_one_of_len);

        if let Some(inner_expected) = expected_inner_type {
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

        with_settings!({ snapshot_path => "snapshots", snapshot_suffix => format!("tuple_named_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r"
        enum Mixed {
            Ready,
            Data(String),
        }
        ",
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing for mixed enum");
        assert_eq!(one_of.len(), expected_one_of_len);

        let SchemaRef::Inline(unit_schema) = &one_of[0] else {
            panic!("Expected inline schema for unit variant")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props.get("detail").expect("variant key missing") else {
            panic!("Expected inline inner schema")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props
            .get("detail")
            .or_else(|| props.get("Detail"))
            .expect("variant key missing")
        else {
            panic!("Expected inline inner schema")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");

        // Check UserCreated variant key is camelCase
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("userCreated"));
        assert!(!props.contains_key("UserCreated"));
        assert!(!props.contains_key("user_created"));

        // Check UserDeleted variant key is camelCase
        let SchemaRef::Inline(variant_obj2) = &one_of[1] else {
            panic!("Expected inline schema")
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

        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let enum_values = schema.r#enum.expect("enum values missing");
        assert_eq!(enum_values[0].as_str().unwrap(), "HIGH_PRIORITY");
        assert_eq!(enum_values[1].as_str().unwrap(), "LOW_PRIORITY");
    }

    // Test enum with empty variants (edge case)
    #[test]
    fn test_parse_enum_to_schema_empty_enum() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r"
            enum Empty {}
        ",
        )
        .unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        // Empty enum should have no enum values
        assert!(schema.r#enum.is_none() || schema.r#enum.as_ref().unwrap().is_empty());
    }

    // Test enum with all struct variants having empty properties
    #[test]
    fn test_parse_enum_to_schema_struct_variant_no_fields() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r"
            enum Event {
                Empty {},
            }
        ",
        )
        .unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        assert_eq!(one_of.len(), 1);
    }

    // Tests for enum with doc comments on variants
    #[test]
    fn test_parse_enum_to_schema_with_variant_descriptions() {
        let enum_src = r"
            /// Enum description
            enum Status {
                /// Active variant
                Active,
                /// Inactive variant
                Inactive,
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
        assert_eq!(schema.description, Some("Enum description".to_string()));
    }

    #[test]
    fn test_parse_enum_to_schema_data_variant_with_description() {
        let enum_src = r"
            /// Data enum
            enum Event {
                /// Text event description
                Text(String),
                /// Number event description
                Number(i32),
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
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
        let enum_src = r"
            enum Event {
                /// Record variant
                Record {
                    /// The value field
                    value: i32,
                    /// The name field
                    name: String,
                },
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashSet::new(), &HashMap::new());
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
            r"
            enum Message {
                Data {
                    /// The user associated with this message
                    user: User,
                },
            }
        ",
        )
        .unwrap();

        // Register User as a known schema to get SchemaRef::Ref
        let mut known_schemas = HashSet::new();
        known_schemas.insert("User".to_string());

        let schema = parse_enum_to_schema(&enum_item, &known_schemas, &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");

        // Get the Data variant schema
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props.get("Data").expect("variant key missing") else {
            panic!("Expected inline inner schema")
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
                let SchemaRef::Ref(reference) = &all_of[0] else {
                    panic!("Expected $ref in allOf")
                };
                assert_eq!(reference.ref_path, "#/components/schemas/User");
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema with allOf, not direct $ref"),
        }
    }
}
