use std::collections::{HashMap, HashSet};

use crate::parser::schema::enum_schema::parse_enum_to_schema;
use insta::{assert_debug_snapshot, with_settings};
use vespera_core::schema::{SchemaRef, SchemaType};

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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
        r"
                #[serde(untagged)]
                enum StringOrInt {
                    String(String),
                    Int(i32),
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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
        r"
                #[serde(untagged)]
                enum Data {
                    User { name: String, age: i32 },
                    Product { title: String, price: f64 },
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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
        r"
                #[serde(untagged)]
                enum MaybeValue {
                    Nothing,
                    Something(i32),
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );
    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "internally_tagged" }, {
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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );
    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "adjacently_tagged" }, {
        assert_debug_snapshot!(schema);
    });
}

#[test]
fn test_untagged_snapshot() {
    let enum_item: syn::ItemEnum = syn::parse_str(
        r"
                #[serde(untagged)]
                enum Value {
                    Null,
                    Bool(bool),
                    Number(f64),
                    Text(String),
                    Object { key: String, value: String },
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );
    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "untagged" }, {
        assert_debug_snapshot!(schema);
    });
}

// Edge case: Empty struct variant (empty properties/required)
#[test]
fn test_externally_tagged_empty_struct_variant() {
    let enum_item: syn::ItemEnum = syn::parse_str(
        r"
                enum Event {
                    /// Empty struct variant
                    Empty {},
                    Data { value: i32 },
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

    let one_of = schema.clone().one_of.expect("one_of missing");
    assert_eq!(one_of.len(), 2);

    // Empty variant should have properties with Empty key pointing to object with no properties
    if let SchemaRef::Inline(empty_variant) = &one_of[0] {
        let props = empty_variant
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props.get("Empty").expect("Empty key missing") else {
            panic!("Expected inline schema")
        };
        // Empty struct should have properties: None and required: None
        assert!(inner.properties.is_none());
        assert!(inner.required.is_none());
    }

    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "externally_tagged_empty_struct" }, {
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

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

    // Tuple variant `Number(i32)` should be skipped, only 2 variants should remain
    let one_of = schema.clone().one_of.expect("one_of missing");
    assert_eq!(one_of.len(), 2); // Text and Empty only

    // Verify discriminator is present
    let discriminator = schema
        .discriminator
        .as_ref()
        .expect("discriminator missing");
    assert_eq!(discriminator.property_name, "type");

    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "internally_tagged_skip_tuple" }, {
        assert_debug_snapshot!(schema);
    });
}

// Edge case: Untagged enum with tuple variant referencing a known schema
#[test]
fn test_untagged_tuple_variant_with_known_schema_ref() {
    let enum_item: syn::ItemEnum = syn::parse_str(
        r"
                #[serde(untagged)]
                enum Payload {
                    User(UserData),
                    Simple(String),
                }
                ",
    )
    .unwrap();

    // Provide UserData as a known schema so it returns SchemaRef::Ref
    let mut known_schemas = HashSet::new();
    known_schemas.insert("UserData".to_string());

    let schema = parse_enum_to_schema(
        &enum_item,
        &known_schemas,
        &HashMap::<String, String>::new(),
    );

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
        r"
                #[serde(untagged)]
                enum Message {
                    Text(String),
                    Pair(i32, String),
                    Triple(i32, String, bool),
                }
                ",
    )
    .unwrap();

    let schema = parse_enum_to_schema(
        &enum_item,
        &HashSet::<String>::new(),
        &HashMap::<String, String>::new(),
    );

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

    with_settings!({ snapshot_path => "../../snapshots", snapshot_suffix => "untagged_multi_field_tuple" }, {
        assert_debug_snapshot!(schema);
    });
}
