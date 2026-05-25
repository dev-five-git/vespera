//! Struct to JSON Schema conversion for `OpenAPI` generation.
//!
//! This module handles the conversion of Rust structs (as parsed by syn)
//! into OpenAPI-compatible JSON Schema definitions.

use std::collections::{BTreeMap, HashMap, HashSet};

use syn::{Fields, Type};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::{
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{
        extract_doc_comment, extract_field_rename, extract_flatten, extract_rename_all,
        extract_schema_ref_override, extract_skip, extract_transparent, rename_field,
        strip_raw_prefix_owned,
    },
    type_schema::parse_type_to_schema_ref,
};

/// Parses a Rust struct into an `OpenAPI` Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields (non-Option types without defaults)
/// - Doc comments as descriptions
/// - Serde attributes (rename, `rename_all`, skip, default)
///
/// # Arguments
/// * `struct_item` - The parsed struct from syn
/// * `known_schemas` - Map of known schema names for reference resolution
/// * `struct_definitions` - Map of struct names to their source code (for generics)
#[allow(clippy::too_many_lines)]
pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut properties = BTreeMap::new();
    let mut required = Vec::with_capacity(8);
    let mut flattened_refs: Vec<SchemaRef> = Vec::new();

    // Extract struct-level doc comment for schema description
    let struct_description = extract_doc_comment(&struct_item.attrs);

    if let Some((schema_name, nullable)) = extract_schema_ref_override(&struct_item.attrs) {
        return Schema {
            ref_path: Some(format!("#/components/schemas/{schema_name}")),
            nullable: nullable.then_some(true),
            description: struct_description,
            ..Default::default()
        };
    }

    // Transparent single-field wrappers should use the inner field schema directly.
    if extract_transparent(&struct_item.attrs) {
        let inner_field_ty = match &struct_item.fields {
            Fields::Named(fields_named) if fields_named.named.len() == 1 => {
                fields_named.named.first().map(|field| &field.ty)
            }
            Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
                fields_unnamed.unnamed.first().map(|field| &field.ty)
            }
            _ => None,
        };

        if let Some(field_ty) = inner_field_ty {
            let schema_ref = parse_type_to_schema_ref(field_ty, known_schemas, struct_definitions);
            return match schema_ref {
                SchemaRef::Inline(mut schema) => {
                    if schema.description.is_none() {
                        schema.description = struct_description;
                    }
                    *schema
                }
                SchemaRef::Ref(reference) => Schema {
                    description: struct_description,
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                },
            };
        }
    }

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

                let rust_field_name = field.ident.as_ref().map_or_else(
                    || "unknown".to_string(),
                    |i| strip_raw_prefix_owned(i.to_string()),
                );

                // Check for field-level rename attribute first (takes precedence)
                let field_name = extract_field_rename(&field.attrs)
                    .unwrap_or_else(|| rename_field(&rust_field_name, rename_all.as_deref()));

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

                // Extract field-level `#[schema(min_length=..., pattern=...,
                // minimum=..., format=..., example=..., read_only, ...)]`
                // constraints and merge them into the field schema.  When
                // the field references a component schema via `$ref`, we
                // promote it to an `allOf` wrapper (mirroring the
                // description-on-ref pattern above) so the constraints can
                // sit alongside the reference.
                let constraints = extract_schema_constraints(&field.attrs);
                if !constraints.is_empty() {
                    apply_constraints_to_schema_ref(&mut schema_ref, &constraints);
                }

                // Required is determined solely by nullability (Option<T>).
                // Fields with #[serde(default)] still have defaults applied in
                // openapi_generator, but that does NOT affect required status.
                let is_optional = matches!(
                    field_type,
                    Type::Path(type_path)
                        if type_path
                            .path
                            .segments
                            .first()
                            .is_some_and(|s| s.ident == "Option")
                );

                if !is_optional {
                    required.push(field_name.clone());
                }

                properties.insert(field_name, schema_ref);
            }
        }
        Fields::Unnamed(_) | Fields::Unit => {
            // Tuple structs and unit structs have no named fields
        }
    }

    // If there are flattened fields, use allOf composition
    if flattened_refs.is_empty() {
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
    } else {
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
    }
}

/// Merge field-level `#[schema(...)]` constraints into the field's
/// `SchemaRef`.  For `Inline` variants the constraints are written
/// directly onto the inner `Schema`; for `Ref` variants we promote to an
/// `allOf` wrapper so the constraints can sit alongside `$ref`.
fn apply_constraints_to_schema_ref(schema_ref: &mut SchemaRef, c: &SchemaConstraints) {
    match schema_ref {
        SchemaRef::Inline(schema) => apply_constraints(schema, c),
        SchemaRef::Ref(_) => {
            // mem::replace lets us move the Ref out without leaving an
            // invalid value behind; the placeholder is overwritten
            // before the function returns.
            let taken =
                std::mem::replace(schema_ref, SchemaRef::Inline(Box::new(Schema::object())));
            if let SchemaRef::Ref(reference) = taken {
                let mut wrapper = Schema {
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                };
                apply_constraints(&mut wrapper, c);
                *schema_ref = SchemaRef::Inline(Box::new(wrapper));
            }
        }
    }
}

/// Apply each set constraint to the corresponding `Schema` field.
fn apply_constraints(schema: &mut Schema, c: &SchemaConstraints) {
    if let Some(v) = c.min_length {
        schema.min_length = Some(v);
    }
    if let Some(v) = c.max_length {
        schema.max_length = Some(v);
    }
    if let Some(ref v) = c.pattern {
        schema.pattern = Some(v.clone());
    }
    if let Some(v) = c.minimum {
        schema.minimum = Some(v);
    }
    if let Some(v) = c.maximum {
        schema.maximum = Some(v);
    }
    if let Some(v) = c.exclusive_minimum {
        schema.exclusive_minimum = Some(v);
    }
    if let Some(v) = c.exclusive_maximum {
        schema.exclusive_maximum = Some(v);
    }
    if let Some(v) = c.multiple_of {
        schema.multiple_of = Some(v);
    }
    if let Some(v) = c.min_items {
        schema.min_items = Some(v);
    }
    if let Some(v) = c.max_items {
        schema.max_items = Some(v);
    }
    if let Some(v) = c.unique_items {
        schema.unique_items = Some(v);
    }
    if let Some(ref v) = c.format {
        schema.format = Some(v.clone());
    }
    if let Some(ref v) = c.example {
        schema.example = Some(v.clone());
    }
    if let Some(v) = c.read_only {
        schema.read_only = Some(v);
    }
    if let Some(v) = c.write_only {
        schema.write_only = Some(v);
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn test_parse_struct_to_schema_required_optional() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            struct User {
                id: i32,
                name: Option<String>,
            }
        ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
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

        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
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
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }

    #[test]
    fn test_parse_struct_to_schema_serde_transparent_named_wrapper_uses_inner_schema() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            #[serde(transparent)]
            struct Wrapper {
                value: Box<String>,
            }
        ",
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert_eq!(schema.schema_type, Some(SchemaType::String));
        assert!(schema.properties.is_none());
    }

    #[test]
    fn test_parse_struct_to_schema_schema_ref_override() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[schema(ref = "UserSchema", nullable)]
            struct Wrapper {
                value: Option<String>,
            }
        "#,
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert_eq!(
            schema.ref_path.as_deref(),
            Some("#/components/schemas/UserSchema")
        );
        assert_eq!(schema.nullable, Some(true));
    }

    // Test struct with skip field
    #[test]
    fn test_parse_struct_to_schema_with_skip_field() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            struct User {
                id: i32,
                #[serde(skip)]
                internal_data: String,
                name: String,
            }
        ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(!props.contains_key("internal_data")); // Should be skipped
    }

    // Test struct with default and skip_serializing_if
    // Required is determined solely by nullability (Option<T>), not by defaults.
    #[test]
    fn test_parse_struct_to_schema_with_default_fields() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Config {
                required_field: i32,
                #[serde(default)]
                with_default: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                maybe_skip: Option<i32>,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("required_field"));
        assert!(props.contains_key("with_default"));
        assert!(props.contains_key("maybe_skip"));

        let required = schema.required.as_ref().unwrap();
        assert!(required.contains(&"required_field".to_string()));
        // Non-nullable fields are always required, even with #[serde(default)]
        assert!(required.contains(&"with_default".to_string()));
        // Option<T> fields are not required (nullable)
        assert!(!required.contains(&"maybe_skip".to_string()));
    }

    // Tests for struct with doc comments
    #[test]
    fn test_parse_struct_to_schema_with_description() {
        let struct_src = r"
            /// User struct description
            struct User {
                /// User ID
                id: i32,
                /// User name
                name: String,
            }
        ";
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
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
        let struct_src = r"
            struct Container {
                /// The user reference
                user: User,
            }
        ";
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let mut struct_defs = HashMap::new();
        struct_defs.insert("User".to_string(), "struct User { id: i32 }".to_string());
        let mut known = HashSet::new();
        known.insert("User".to_string());
        let schema = parse_struct_to_schema(&struct_item, &known, &struct_defs);
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
    fn test_parse_struct_to_schema_description_strips_slash_prefix() {
        // When doc attributes have "/ " prefix (without leading space), descriptions should be clean.
        // This can happen in certain TokenStream roundtrip scenarios.
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[doc = "/ Struct description"]
            struct Admin {
                #[doc = "/ Field description"]
                id: i32,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert_eq!(schema.description, Some("Struct description".to_string()));
        let props = schema.properties.unwrap();
        if let SchemaRef::Inline(id_schema) = props.get("id").unwrap() {
            assert_eq!(id_schema.description, Some("Field description".to_string()));
        }
    }

    #[test]
    fn test_parse_struct_to_schema_with_flatten() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            struct UserListRequest {
                filter: String,
                #[serde(flatten)]
                pagination: Pagination,
            }
        ",
        )
        .unwrap();

        let mut struct_defs = HashMap::new();
        struct_defs.insert(
            "Pagination".to_string(),
            "struct Pagination { page: i32 }".to_string(),
        );
        let mut known = HashSet::new();
        known.insert("Pagination".to_string());

        let schema = parse_struct_to_schema(&struct_item, &known, &struct_defs);

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
            r"
            struct Combined {
                name: String,
                #[serde(flatten)]
                pagination: Pagination,
                #[serde(flatten)]
                metadata: Metadata,
            }
        ",
        )
        .unwrap();

        let mut struct_defs = HashMap::new();
        struct_defs.insert("Pagination".to_string(), "struct Pagination {}".to_string());
        struct_defs.insert("Metadata".to_string(), "struct Metadata {}".to_string());
        let mut known = HashSet::new();
        known.insert("Pagination".to_string());
        known.insert("Metadata".to_string());

        let schema = parse_struct_to_schema(&struct_item, &known, &struct_defs);

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
            r"
            struct Simple {
                name: String,
                age: i32,
            }
        ",
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert!(
            schema.all_of.is_none(),
            "Simple struct should not have allOf"
        );
        assert!(schema.properties.is_some());
    }

    #[test]
    fn test_parse_struct_to_schema_transparent_tuple_wrapper_uses_ref_schema() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            #[serde(transparent)]
            struct Wrapper(User);
        ",
        )
        .unwrap();

        let mut struct_defs = HashMap::new();
        struct_defs.insert("User".to_string(), "struct User { id: i32 }".to_string());
        let mut known = HashSet::new();
        known.insert("User".to_string());

        let schema = parse_struct_to_schema(&struct_item, &known, &struct_defs);
        assert!(schema.all_of.is_some());
        let all_of = schema.all_of.unwrap();
        assert_eq!(all_of.len(), 1);
        match &all_of[0] {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/User");
            }
            SchemaRef::Inline(_) => {
                panic!("expected $ref wrapper for transparent tuple known schema")
            }
        }
    }

    #[test]
    fn test_parse_struct_to_schema_transparent_multi_field_tuple_falls_back() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            #[serde(transparent)]
            struct Wrapper(String, String);
        ",
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        assert_eq!(schema.schema_type, Some(SchemaType::Object));
        assert!(schema.properties.is_none());
        assert!(schema.all_of.is_none());
    }

    // ── field-level `#[schema(...)]` constraint propagation ─────────

    fn field_schema<'a>(schema: &'a Schema, field: &str) -> &'a Schema {
        let props = schema.properties.as_ref().expect("properties missing");
        let entry = props.get(field).expect("field missing");
        match entry {
            SchemaRef::Inline(boxed) => boxed.as_ref(),
            SchemaRef::Ref(_) => panic!("expected inline schema for field '{field}'"),
        }
    }

    #[test]
    fn schema_constraints_min_max_length_and_pattern_on_string_field() {
        let s: syn::ItemStruct = syn::parse_str(
            r#"
            struct CreateUser {
                #[schema(min_length = 3, max_length = 32, pattern = "^[a-z]+$")]
                username: String,
            }
            "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "username");
        assert_eq!(field.min_length, Some(3));
        assert_eq!(field.max_length, Some(32));
        assert_eq!(field.pattern.as_deref(), Some("^[a-z]+$"));
    }

    #[test]
    fn schema_constraints_minimum_maximum_on_numeric_field() {
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Profile {
                #[schema(minimum = 0, maximum = 150)]
                age: u32,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "age");
        assert_eq!(field.minimum, Some(0.0));
        assert_eq!(field.maximum, Some(150.0));
    }

    #[test]
    fn schema_constraints_format_email_on_string_field() {
        let s: syn::ItemStruct = syn::parse_str(
            r#"
            struct Contact {
                #[schema(format = "email")]
                email: String,
            }
            "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "email");
        assert_eq!(field.format.as_deref(), Some("email"));
    }

    #[test]
    fn schema_constraints_read_only_write_only_example() {
        let s: syn::ItemStruct = syn::parse_str(
            r#"
            struct User {
                #[schema(read_only, example = "abc-123")]
                id: String,
                #[schema(write_only)]
                password: String,
            }
            "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let id_field = field_schema(&schema, "id");
        assert_eq!(id_field.read_only, Some(true));
        assert_eq!(id_field.example, Some(serde_json::json!("abc-123")));
        let pw_field = field_schema(&schema, "password");
        assert_eq!(pw_field.write_only, Some(true));
    }

    #[test]
    fn schema_constraints_min_max_items_unique_on_vec_field() {
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Post {
                #[schema(min_items = 1, max_items = 5, unique_items)]
                tags: Vec<String>,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "tags");
        assert_eq!(field.min_items, Some(1));
        assert_eq!(field.max_items, Some(5));
        assert_eq!(field.unique_items, Some(true));
    }

    #[test]
    fn schema_constraints_exclusive_bounds_and_multiple_of() {
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Price {
                #[schema(minimum = 0, exclusive_minimum, multiple_of = 0.01)]
                amount: f64,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "amount");
        assert_eq!(field.minimum, Some(0.0));
        assert_eq!(field.exclusive_minimum, Some(true));
        assert_eq!(field.multiple_of, Some(0.01));
    }

    #[test]
    fn schema_constraints_on_ref_field_promote_to_allof_wrapper() {
        // A field referencing a known component schema must keep its
        // `$ref` but gain the constraints via an `allOf` wrapper so the
        // OpenAPI consumer still sees the reference.
        let mut known = HashSet::new();
        known.insert("Address".to_string());
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Order {
                #[schema(read_only)]
                shipping: Address,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &known, &HashMap::new());
        let field = field_schema(&schema, "shipping");
        assert_eq!(field.read_only, Some(true));
        let all_of = field.all_of.as_ref().expect("allOf wrap missing");
        assert_eq!(all_of.len(), 1);
        assert!(matches!(all_of[0], SchemaRef::Ref(_)));
    }

    #[test]
    fn schema_constraints_coexist_with_doc_comment_on_ref_field() {
        // When BOTH a doc comment AND constraints are present on a
        // `$ref` field, the doc comment converts it to allOf first, then
        // constraints are layered onto the same wrapper.
        let mut known = HashSet::new();
        known.insert("Address".to_string());
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Order {
                /// Shipping address — must be present.
                #[schema(read_only, write_only = false)]
                shipping: Address,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &known, &HashMap::new());
        let field = field_schema(&schema, "shipping");
        assert!(field.description.is_some(), "doc comment lost");
        assert_eq!(field.read_only, Some(true));
        assert_eq!(field.write_only, Some(false));
        assert!(field.all_of.is_some(), "allOf wrap lost");
    }

    #[test]
    fn schema_constraints_unknown_keys_on_field_are_silently_ignored() {
        // Struct-level keys (e.g. `name`) accidentally placed on a field
        // attribute should not trip the parser nor produce constraints.
        let s: syn::ItemStruct = syn::parse_str(
            r#"
            struct Account {
                #[schema(name = "Stray", min_length = 4)]
                pin: String,
            }
            "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let field = field_schema(&schema, "pin");
        assert_eq!(field.min_length, Some(4));
    }
}
