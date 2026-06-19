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

    #[test]
    fn test_parse_struct_to_schema_skip_takes_precedence_over_skip_serializing_if() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct User {
                id: i32,
                #[serde(skip, skip_serializing_if = "Option::is_none")]
                email2: Option<String>,
                name: String,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashSet::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(!props.contains_key("email2"));
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
        assert_eq!(field.exclusive_minimum, Some(0.0));
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

    #[test]
    fn schema_exclusive_maximum_and_minimum_land_on_emitted_field_schema() {
        // `exclusive_minimum` / `exclusive_maximum` / `multiple_of` /
        // `unique_items` are OpenAPI-only annotations (no garde rule
        // counterpart).  The struct-schema parser still propagates them
        // onto the per-field `Schema` so the resulting `openapi.json`
        // carries them verbatim.
        let s: syn::ItemStruct = syn::parse_str(
            r"
            struct Price {
                #[schema(minimum = 0, maximum = 100, exclusive_minimum, exclusive_maximum, multiple_of = 0.5)]
                amount: f64,

                #[schema(min_items = 1, max_items = 5, unique_items)]
                tags: Vec<String>,
            }
            ",
        )
        .unwrap();
        let schema = parse_struct_to_schema(&s, &HashSet::new(), &HashMap::new());
        let amount = field_schema(&schema, "amount");
        assert_eq!(amount.exclusive_minimum, Some(0.0));
        assert_eq!(amount.exclusive_maximum, Some(100.0));
        assert_eq!(amount.multiple_of, Some(0.5));
        let tags = field_schema(&schema, "tags");
        assert_eq!(tags.unique_items, Some(true));
    }
