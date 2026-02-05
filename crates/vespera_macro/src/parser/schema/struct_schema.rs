//! Struct to JSON Schema conversion for OpenAPI generation.
//!
//! This module handles the conversion of Rust structs (as parsed by syn)
//! into OpenAPI-compatible JSON Schema definitions.

use std::collections::{BTreeMap, HashMap};

use syn::{Fields, Type};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::{
    serde_attrs::{
        extract_default, extract_doc_comment, extract_field_rename, extract_flatten,
        extract_rename_all, extract_skip, extract_skip_serializing_if, rename_field,
        strip_raw_prefix,
    },
    type_schema::parse_type_to_schema_ref,
};

/// Parses a Rust struct into an OpenAPI Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields (non-Option types without defaults)
/// - Doc comments as descriptions
/// - Serde attributes (rename, rename_all, skip, default)
///
/// # Arguments
/// * `struct_item` - The parsed struct from syn
/// * `known_schemas` - Map of known schema names for reference resolution
/// * `struct_definitions` - Map of struct names to their source code (for generics)
pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut properties = BTreeMap::new();
    let mut required = Vec::new();
    let mut flattened_refs: Vec<SchemaRef> = Vec::new();

    // Extract struct-level doc comment for schema description
    let struct_description = extract_doc_comment(&struct_item.attrs);

    // Extract rename_all attribute from struct
    let rename_all = extract_rename_all(&struct_item.attrs);

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            for field in &fields_named.named {
                // Check if field should be skipped
                if extract_skip(&field.attrs) {
                    continue;
                }

                // Check if field should be flattened
                if extract_flatten(&field.attrs) {
                    // Get the schema ref for the flattened field type
                    let field_type = &field.ty;
                    let schema_ref =
                        parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                    // Add to flattened refs for allOf composition
                    flattened_refs.push(schema_ref);
                    continue;
                }

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
                    rename_field(&rust_field_name, rename_all.as_deref())
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
                            // For $ref schemas, we need to wrap in an allOf to add description
                            // OpenAPI 3.1 allows siblings to $ref, so we can add description directly
                            // by converting to inline schema with description + allOf[$ref]
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

                // Check for default attribute
                let has_default = extract_default(&field.attrs).is_some();

                // Check for skip_serializing_if attribute
                let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);

                // If default or skip_serializing_if is present, mark field as optional (not required)
                // and set default value if it's a simple default (not a function)
                if has_default || has_skip_serializing_if {
                    // For default = "function_name", we'll handle it in openapi_generator
                    // For now, just mark as optional
                    if let SchemaRef::Inline(ref mut _schema) = schema_ref {
                        // Default will be set later in openapi_generator if it's a function
                        // For simple default, we could set it here, but serde handles it
                    }
                } else {
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
                        required.push(field_name.clone());
                    }
                }

                properties.insert(field_name, schema_ref);
            }
        }
        Fields::Unnamed(_) => {
            // Tuple structs are not supported for now
        }
        Fields::Unit => {
            // Unit structs have no fields
        }
    }

    // If there are flattened fields, use allOf composition
    if !flattened_refs.is_empty() {
        // Create the inline schema for non-flattened properties
        let inline_schema = Schema {
            schema_type: Some(SchemaType::Object),
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
        };

        // Build allOf: [inline_schema, ...flattened_refs]
        let mut all_of = vec![SchemaRef::Inline(Box::new(inline_schema))];
        all_of.extend(flattened_refs);

        Schema {
            description: struct_description,
            all_of: Some(all_of),
            ..Default::default()
        }
    } else {
        // No flattened fields - return normal schema
        Schema {
            schema_type: Some(SchemaType::Object),
            description: struct_description,
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
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn test_parse_struct_to_schema_required_optional() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct User {
                id: i32,
                name: Option<String>,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(
            schema
                .required
                .as_ref()
                .unwrap()
                .contains(&"id".to_string())
        );
        assert!(
            !schema
                .required
                .as_ref()
                .unwrap()
                .contains(&"name".to_string())
        );
    }

    #[test]
    fn test_parse_struct_to_schema_rename_all_and_field_rename() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase")]
            struct Profile {
                #[serde(rename = "id")]
                user_id: i32,
                display_name: Option<String>,
            }
        "#,
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().expect("props missing");
        assert!(props.contains_key("id")); // field-level rename wins
        assert!(props.contains_key("displayName")); // rename_all applied
        let required = schema.required.as_ref().expect("required missing");
        assert!(required.contains(&"id".to_string()));
        assert!(!required.contains(&"displayName".to_string())); // Option makes it optional
    }

    #[rstest]
    #[case("struct Wrapper(i32);")]
    #[case("struct Empty;")]
    fn test_parse_struct_to_schema_tuple_and_unit_structs(#[case] struct_src: &str) {
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }

    // Test struct with skip field
    #[test]
    fn test_parse_struct_to_schema_with_skip_field() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct User {
                id: i32,
                #[serde(skip)]
                internal_data: String,
                name: String,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(!props.contains_key("internal_data")); // Should be skipped
    }

    // Test struct with default and skip_serializing_if
    #[test]
    fn test_parse_struct_to_schema_with_default_fields() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Config {
                required_field: i32,
                #[serde(default)]
                optional_with_default: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                maybe_skip: Option<i32>,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("required_field"));
        assert!(props.contains_key("optional_with_default"));
        assert!(props.contains_key("maybe_skip"));

        let required = schema.required.as_ref().unwrap();
        assert!(required.contains(&"required_field".to_string()));
        // Fields with default should NOT be required
        assert!(!required.contains(&"optional_with_default".to_string()));
        // Fields with skip_serializing_if should NOT be required
        assert!(!required.contains(&"maybe_skip".to_string()));
    }

    // Tests for struct with doc comments
    #[test]
    fn test_parse_struct_to_schema_with_description() {
        let struct_src = r#"
            /// User struct description
            struct User {
                /// User ID
                id: i32,
                /// User name
                name: String,
            }
        "#;
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        assert_eq!(
            schema.description,
            Some("User struct description".to_string())
        );
        // Check field descriptions
        let props = schema.properties.unwrap();
        if let SchemaRef::Inline(id_schema) = props.get("id").unwrap() {
            assert_eq!(id_schema.description, Some("User ID".to_string()));
        }
        if let SchemaRef::Inline(name_schema) = props.get("name").unwrap() {
            assert_eq!(name_schema.description, Some("User name".to_string()));
        }
    }

    #[test]
    fn test_parse_struct_to_schema_field_with_ref_and_description() {
        let struct_src = r#"
            struct Container {
                /// The user reference
                user: User,
            }
        "#;
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let mut known = HashMap::new();
        known.insert("User".to_string(), "struct User { id: i32 }".to_string());
        let schema = parse_struct_to_schema(&struct_item, &known, &HashMap::new());
        let props = schema.properties.unwrap();
        // Field with $ref and description should use allOf
        if let SchemaRef::Inline(user_schema) = props.get("user").unwrap() {
            assert_eq!(
                user_schema.description,
                Some("The user reference".to_string())
            );
            assert!(user_schema.all_of.is_some());
        }
    }

    #[test]
    fn test_parse_struct_to_schema_with_flatten() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct UserListRequest {
                filter: String,
                #[serde(flatten)]
                pagination: Pagination,
            }
        "#,
        )
        .unwrap();

        let mut known = HashMap::new();
        known.insert(
            "Pagination".to_string(),
            "struct Pagination { page: i32 }".to_string(),
        );

        let schema = parse_struct_to_schema(&struct_item, &known, &HashMap::new());

        // Should have allOf
        assert!(
            schema.all_of.is_some(),
            "Schema should have allOf for flatten"
        );
        let all_of = schema.all_of.as_ref().unwrap();
        assert_eq!(all_of.len(), 2, "allOf should have 2 elements");

        // First element should be the object with non-flattened properties
        if let SchemaRef::Inline(obj_schema) = &all_of[0] {
            let props = obj_schema.properties.as_ref().unwrap();
            assert!(props.contains_key("filter"), "Should have filter property");
            assert!(
                !props.contains_key("pagination"),
                "Should NOT have pagination property"
            );
        } else {
            panic!("First allOf element should be inline schema");
        }

        // Second element should be $ref to Pagination
        if let SchemaRef::Ref(reference) = &all_of[1] {
            assert_eq!(reference.ref_path, "#/components/schemas/Pagination");
        } else {
            panic!("Second allOf element should be $ref");
        }
    }

    #[test]
    fn test_parse_struct_to_schema_with_multiple_flatten() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Combined {
                name: String,
                #[serde(flatten)]
                pagination: Pagination,
                #[serde(flatten)]
                metadata: Metadata,
            }
        "#,
        )
        .unwrap();

        let mut known = HashMap::new();
        known.insert("Pagination".to_string(), "struct Pagination {}".to_string());
        known.insert("Metadata".to_string(), "struct Metadata {}".to_string());

        let schema = parse_struct_to_schema(&struct_item, &known, &HashMap::new());

        assert!(schema.all_of.is_some());
        let all_of = schema.all_of.as_ref().unwrap();
        assert_eq!(
            all_of.len(),
            3,
            "allOf should have 3 elements (1 inline + 2 refs)"
        );
    }

    #[test]
    fn test_parse_struct_to_schema_no_flatten() {
        // Existing struct without flatten should NOT use allOf
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Simple {
                name: String,
                age: i32,
            }
        "#,
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        assert!(
            schema.all_of.is_none(),
            "Simple struct should not have allOf"
        );
        assert!(schema.properties.is_some());
    }
}
