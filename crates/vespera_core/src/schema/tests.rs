use super::*;
use rstest::rstest;

#[rstest]
#[case(Schema::string(), SchemaType::String)]
#[case(Schema::integer(), SchemaType::Integer)]
#[case(Schema::number(), SchemaType::Number)]
#[case(Schema::boolean(), SchemaType::Boolean)]
fn primitive_helpers_set_schema_type(#[case] schema: Schema, #[case] expected: SchemaType) {
    assert_eq!(schema.schema_type, Some(expected));
}

#[test]
fn array_helper_sets_type_and_items() {
    let item_schema = Schema::boolean();
    let schema = Schema::array(SchemaRef::Inline(Box::new(item_schema.clone())));

    assert_eq!(schema.schema_type, Some(SchemaType::Array));
    let items = schema.items.expect("items should be set");
    match items {
        SchemaRef::Inline(inner) => {
            assert_eq!(inner.schema_type, Some(SchemaType::Boolean));
        }
        SchemaRef::Ref(_) => panic!("array helper should set inline items"),
    }
}

#[test]
fn object_helper_initializes_collections() {
    let schema = Schema::object();

    assert_eq!(schema.schema_type, Some(SchemaType::Object));
    let props = schema.properties.expect("properties should be initialized");
    assert!(props.is_empty());
    let required = schema.required.expect("required should be initialized");
    assert!(required.is_empty());
}

#[test]
fn object_empty_helper_avoids_empty_collection_allocations() {
    let schema = Schema::object_empty();

    assert_eq!(schema.schema_type, Some(SchemaType::Object));
    assert!(schema.properties.is_none());
    assert!(schema.required.is_none());
    assert_eq!(
        serde_json::to_string(&schema).unwrap(),
        r#"{"type":"object"}"#
    );
}

#[test]
fn serialize_number_constraint_none_serializes_null() {
    // Direct call bypasses skip_serializing_if to cover the None branch
    let result = super::serialize_number_constraint(&None, serde_json::value::Serializer).unwrap();
    assert_eq!(result, serde_json::Value::Null);
}

#[rstest]
#[case(2.0, true)]
#[case(1.5, false)]
#[case(1e20, false)]
fn serialize_number_constraint_preserves_value_and_uses_safe_integer_form(
    #[case] number: f64,
    #[case] should_be_integer: bool,
) {
    let result =
        super::serialize_number_constraint(&Some(number), serde_json::value::Serializer).unwrap();

    assert_eq!(result.as_f64(), Some(number));
    assert_eq!(result.is_i64(), should_be_integer);
}

#[test]
fn serialize_minimum_whole_number_as_integer() {
    let schema = Schema {
        minimum: Some(0.0),
        ..Schema::integer()
    };
    let json = serde_json::to_string(&schema).unwrap();
    // Must be "minimum":0 (integer), NOT "minimum":0.0
    assert!(
        json.contains("\"minimum\":0"),
        "expected integer 0, got: {json}"
    );
    assert!(
        !json.contains("\"minimum\":0.0"),
        "must not contain 0.0: {json}"
    );
}

#[test]
fn serialize_minimum_fractional_as_float() {
    let schema = Schema {
        minimum: Some(1.5),
        ..Schema::number()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"minimum\":1.5"),
        "expected 1.5, got: {json}"
    );
}

#[test]
fn serialize_minimum_none_omitted() {
    let schema = Schema::integer();
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        !json.contains("minimum"),
        "None minimum should be omitted: {json}"
    );
}

#[test]
fn schema_level_example_serializes_as_examples_array() {
    let schema = Schema {
        example: Some(serde_json::json!("abc")),
        examples: Some(vec![serde_json::json!("def")]),
        ..Schema::string()
    };

    let value: serde_json::Value = serde_json::to_value(schema).unwrap();

    assert!(value.get("example").is_none());
    assert_eq!(value["examples"], serde_json::json!(["abc", "def"]));
}

#[test]
fn schema_level_example_and_examples_serialization_is_byte_identical() {
    let schema = Schema {
        example: Some(serde_json::json!("abc")),
        examples: Some(vec![serde_json::json!("def")]),
        ..Schema::string()
    };

    let json = serde_json::to_string(&schema).unwrap();

    assert_eq!(json, r#"{"type":"string","examples":["abc","def"]}"#);
}

#[test]
fn schema_level_legacy_example_alone_serializes_as_single_element_examples_array() {
    let schema = Schema {
        example: Some(serde_json::json!("legacy")),
        ..Schema::string()
    };

    let value = serde_json::to_value(schema).unwrap();

    assert_eq!(value["examples"], serde_json::json!(["legacy"]));
    assert!(value.get("example").is_none());
}

#[test]
fn schema_level_examples_without_legacy_example_are_preserved() {
    let schema = Schema {
        examples: Some(vec![
            serde_json::json!("first"),
            serde_json::json!("second"),
        ]),
        ..Schema::string()
    };

    let value = serde_json::to_value(schema).unwrap();

    assert_eq!(value["examples"], serde_json::json!(["first", "second"]));
}

#[test]
fn schema_level_legacy_example_deserializes_for_round_trip_compatibility() {
    let schema: Schema = serde_json::from_value(serde_json::json!({
        "type": "string",
        "example": "legacy"
    }))
    .unwrap();

    assert_eq!(schema.example, Some(serde_json::json!("legacy")));
}

#[test]
fn serialize_maximum_whole_number_as_integer() {
    let schema = Schema {
        maximum: Some(100.0),
        ..Schema::integer()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"maximum\":100"),
        "expected integer 100, got: {json}"
    );
    assert!(
        !json.contains("\"maximum\":100.0"),
        "must not contain 100.0: {json}"
    );
}

#[test]
fn serialize_out_of_i64_range_constraint_stays_float() {
    // A whole-number constraint beyond i64 range must NOT saturate to
    // i64::MAX — it stays a float so the spec keeps the real value.
    let schema = Schema {
        maximum: Some(1e20),
        ..Schema::number()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        !json.contains(&i64::MAX.to_string()),
        "must not saturate to i64::MAX: {json}"
    );
    // Parse back: the constraint value must be preserved exactly,
    // regardless of serde's float formatting.
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed["maximum"].as_f64(),
        Some(1e20),
        "constraint value must be preserved: {json}"
    );
}

#[test]
fn serialize_multiple_of_whole_number_as_integer() {
    let schema = Schema {
        multiple_of: Some(2.0),
        ..Schema::integer()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"multipleOf\":2"),
        "expected integer 2, got: {json}"
    );
    assert!(
        !json.contains("\"multipleOf\":2.0"),
        "must not contain 2.0: {json}"
    );
}

// ── CORE: OpenAPI 3.1 conformance of the schema model ────────────

#[test]
fn oauth2_security_scheme_serializes_to_canonical_lowercase() {
    // OpenAPI's canonical wire name is `oauth2`.  serde's `camelCase`
    // container rule lowercases only the leading char, which would emit
    // the invalid `oAuth2` without the explicit `#[serde(rename)]`.
    let json = serde_json::to_string(&SecuritySchemeType::OAuth2).unwrap();
    assert_eq!(json, "\"oauth2\"", "must be exactly \"oauth2\"");
}

#[rstest]
#[case(SecuritySchemeType::ApiKey, "\"apiKey\"")]
#[case(SecuritySchemeType::Http, "\"http\"")]
#[case(SecuritySchemeType::MutualTls, "\"mutualTLS\"")]
#[case(SecuritySchemeType::OAuth2, "\"oauth2\"")]
#[case(SecuritySchemeType::OpenIdConnect, "\"openIdConnect\"")]
fn security_scheme_type_uses_openapi_canonical_wire_names(
    #[case] ty: SecuritySchemeType,
    #[case] expected: &str,
) {
    assert_eq!(serde_json::to_string(&ty).unwrap(), expected);
}

#[test]
fn from_compiled_json_invalid_input_yields_the_sentinel_instead_of_panicking() {
    let schema = Schema::from_compiled_json("{not valid json");

    assert_eq!(schema.title.as_deref(), Some("VESPERA_SCHEMA_PARSE_ERROR"));
    assert!(
        schema
            .description
            .as_deref()
            .is_some_and(|description| description.contains("macro/serde drift")),
        "sentinel description should identify macro/serde drift: {schema:#?}",
    );
}

#[test]
fn from_compiled_json_round_trips_a_macro_emitted_schema() {
    let source = Schema {
        title: Some("User".to_owned()),
        schema_type: Some(SchemaType::Object),
        ..Schema::default()
    };
    let json = serde_json::to_string(&source).unwrap();

    let restored = Schema::from_compiled_json(&json);

    assert_eq!(restored.title.as_deref(), Some("User"));
    assert_eq!(restored.schema_type, Some(SchemaType::Object));
}

#[test]
fn compiled_json_parse_failure_sentinel_is_machine_detectable() {
    let error = serde_json::from_str::<Schema>("{not valid json").unwrap_err();
    let schema = schema_parse_failure_sentinel(&error);

    assert_eq!(schema.title.as_deref(), Some("VESPERA_SCHEMA_PARSE_ERROR"));
    assert!(
        schema
            .description
            .as_deref()
            .is_some_and(|description| description.contains("macro/serde drift")),
        "sentinel description should identify macro/serde drift: {schema:#?}",
    );
}

// ── CORE-04: typed `additionalProperties` (untagged) ─────────────
//
// The untagged enum MUST serialize to the bare JSON Schema wire form
// (a `true`/`false` or the schema object/`$ref`) — byte-identical to
// the previous `serde_json::Value` representation — and round-trip
// back to the right variant.  Untagged deserialization is
// order-sensitive, so these lock the contract.

#[test]
fn additional_properties_bool_serializes_bare() {
    let schema = Schema {
        additional_properties: Some(AdditionalProperties::Bool(false)),
        ..Schema::object_empty()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"additionalProperties\":false"),
        "bool must serialize as a bare boolean, got: {json}"
    );
}

#[test]
fn additional_properties_schema_ref_serializes_as_ref() {
    let schema = Schema {
        additional_properties: Some(AdditionalProperties::Schema(SchemaRef::Ref(
            Reference::schema("User"),
        ))),
        ..Schema::object_empty()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"additionalProperties\":{\"$ref\":\"#/components/schemas/User\"}"),
        "schema-ref must serialize as a bare $ref object, got: {json}"
    );
}

#[test]
fn additional_properties_roundtrips_each_variant() {
    // bool → Bool
    let v: AdditionalProperties = serde_json::from_str("true").unwrap();
    assert!(matches!(v, AdditionalProperties::Bool(true)));
    // {"$ref":...} → Schema(Ref)
    let v: AdditionalProperties =
        serde_json::from_str(r##"{"$ref":"#/components/schemas/X"}"##).unwrap();
    assert!(matches!(v, AdditionalProperties::Schema(SchemaRef::Ref(_))));
    // inline schema object → Schema(Inline)
    let v: AdditionalProperties = serde_json::from_str(r#"{"type":"string"}"#).unwrap();
    assert!(matches!(
        v,
        AdditionalProperties::Schema(SchemaRef::Inline(_))
    ));
}

// ── CORE-03: nullable-reference constructor ──────────────────────

#[test]
fn nullable_reference_emits_anyof_ref_and_null_only() {
    let schema = Schema::nullable_reference("#/components/schemas/User".to_owned());
    let json = serde_json::to_string(&schema).unwrap();
    assert!(
        json.contains("\"anyOf\":[{\"$ref\":\"#/components/schemas/User\"},{\"type\":\"null\"}]"),
        "nullable ref must be anyOf(ref, null): {json}"
    );
    assert!(
        !json.contains("\"nullable\""),
        "OpenAPI 3.1 must not emit nullable: {json}"
    );
    // schema_type stays None so no top-level `"type"` is emitted alongside.
    assert!(
        !json.starts_with("{\"type\":"),
        "a nullable reference must not also emit a top-level type: {json}"
    );
}

#[test]
fn nullable_reference_serialization_is_byte_identical() {
    let schema = Schema::nullable_reference("#/components/schemas/User".to_owned());

    let json = serde_json::to_string(&schema).unwrap();

    assert_eq!(
        json,
        r##"{"anyOf":[{"$ref":"#/components/schemas/User"},{"type":"null"}]}"##
    );
}

#[test]
fn nullable_reference_with_explicit_any_of_returns_clean_serialization_error() {
    let schema = Schema {
        any_of: Some(vec![SchemaRef::Inline(Box::new(Schema::string()))]),
        ..Schema::nullable_reference("#/components/schemas/User".to_owned())
    };

    let err = serde_json::to_string(&schema).unwrap_err();

    assert!(
        err.to_string()
            .contains("cannot also carry explicit any_of"),
        "unexpected error: {err}",
    );
}

#[test]
fn nullable_reference_with_explicit_type_returns_clean_serialization_error() {
    // A hand-built nullable `$ref` that ALSO carries a `schema_type` would
    // serialize both `anyOf` and a sibling `type` — ambiguous/invalid OpenAPI.
    // It must fail with a clean serialization error like the `any_of` case,
    // not silently emit the broken shape. (Vespera's own `nullable_reference`
    // leaves `schema_type` None, so this only guards external manual construction.)
    let schema = Schema {
        schema_type: Some(SchemaType::Object),
        ..Schema::nullable_reference("#/components/schemas/User".to_owned())
    };

    let err = serde_json::to_string(&schema).unwrap_err();

    assert!(
        err.to_string()
            .contains("cannot also carry an explicit type"),
        "unexpected error: {err}",
    );
}

#[test]
fn nullable_primitive_emits_type_array_with_null() {
    let schema = Schema {
        nullable: Some(true),
        ..Schema::string()
    };
    let json = serde_json::to_string(&schema).unwrap();
    assert_eq!(json, r#"{"type":["string","null"]}"#);
}

#[test]
fn nullable_primitive_type_array_deserializes() {
    let schema: Schema = serde_json::from_str(r#"{"type":["integer","null"]}"#).unwrap();
    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
    assert_eq!(schema.nullable, Some(true));
}

#[test]
fn duplicate_single_type_array_deserializes_without_loss() {
    let schema: Schema = serde_json::from_str(r#"{"type":["integer","integer","null"]}"#).unwrap();

    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
    assert_eq!(schema.nullable, Some(true));
}

#[test]
fn null_only_type_array_round_trips_to_singular_null() {
    // Regression: `{"type":["null"]}` previously deserialized to
    // (schema_type=None, nullable=Some(true)) and re-serialized to `{}`,
    // silently dropping the null constraint. It must collapse to the
    // equivalent singular `type:"null"` and round-trip losslessly.
    let schema: Schema = serde_json::from_str(r#"{"type":["null"]}"#).unwrap();
    assert_eq!(schema.schema_type, Some(SchemaType::Null));
    assert_eq!(schema.nullable, None);

    let json = serde_json::to_string(&schema).unwrap();
    assert_eq!(json, r#"{"type":"null"}"#);
}

#[test]
fn repeated_null_only_type_array_round_trips_to_singular_null() {
    let schema: Schema = serde_json::from_str(r#"{"type":["null","null"]}"#).unwrap();
    assert_eq!(schema.schema_type, Some(SchemaType::Null));
    assert_eq!(schema.nullable, None);

    let json = serde_json::to_string(&schema).unwrap();
    assert_eq!(json, r#"{"type":"null"}"#);
}

#[test]
fn multi_type_array_with_null_is_rejected_instead_of_lossy_collapsing() {
    let err =
        serde_json::from_str::<Schema>(r#"{"type":["string","integer","null"]}"#).unwrap_err();

    assert!(
        err.to_string().contains("multiple non-null types"),
        "unexpected error: {err}",
    );
}

#[test]
fn multi_type_array_without_null_is_rejected_instead_of_lossy_collapsing() {
    let err = serde_json::from_str::<Schema>(r#"{"type":["integer","string"]}"#).unwrap_err();

    assert!(
        err.to_string().contains("multiple non-null types"),
        "unexpected error: {err}",
    );
}

#[test]
fn type_array_nullability_wins_over_nullable_false_sibling() {
    let schema: Schema =
        serde_json::from_str(r#"{"type":["string","null"],"nullable":false}"#).unwrap();

    assert_eq!(schema.schema_type, Some(SchemaType::String));
    assert_eq!(schema.nullable, Some(true));
}

#[test]
fn primitive_schema_serialize_contract_stays_byte_identical() {
    assert_eq!(
        serde_json::to_string(&Schema::string()).unwrap(),
        r#"{"type":"string"}"#
    );
}

// ── SchemaRef: $ref-sibling preservation ─────────────────────────
//
// The prior `#[serde(untagged)]` `Ref`-first enum greedily matched
// ANY object with a `$ref` key and silently dropped its siblings
// (e.g. a nullable reference's `"nullable": true`).  The custom
// `Deserialize` treats only a *pure* `{"$ref": <string>}` as a
// reference; a `$ref` with any sibling becomes an inline `Schema`
// so the siblings round-trip intact.

#[test]
fn schema_ref_pure_ref_deserializes_as_ref() {
    let v: SchemaRef = serde_json::from_str(r##"{"$ref":"#/components/schemas/User"}"##).unwrap();
    match v {
        SchemaRef::Ref(r) => assert_eq!(r.ref_path, "#/components/schemas/User"),
        SchemaRef::Inline(_) => panic!("a pure $ref must deserialize as SchemaRef::Ref"),
    }
}

#[test]
fn schema_ref_with_nullable_sibling_preserves_fields() {
    let v: SchemaRef =
        serde_json::from_str(r##"{"$ref":"#/components/schemas/User","nullable":true}"##).unwrap();
    match v {
        SchemaRef::Inline(schema) => {
            assert_eq!(
                schema.ref_path.as_deref(),
                Some("#/components/schemas/User"),
                "the $ref must survive as an inline ref_path"
            );
            assert_eq!(
                schema.nullable,
                Some(true),
                "the nullable sibling must not be dropped"
            );
        }
        SchemaRef::Ref(_) => panic!("$ref with a sibling must not be matched as a bare Ref"),
    }
}

#[test]
fn schema_ref_inline_object_deserializes_as_inline() {
    let v: SchemaRef = serde_json::from_str(r#"{"type":"string"}"#).unwrap();
    assert!(matches!(v, SchemaRef::Inline(_)));
}

#[test]
fn schema_ref_nullable_reference_roundtrips() {
    // Build → serialize → deserialize must keep 3.1 nullable semantics.
    let original = Schema::nullable_reference("#/components/schemas/User".to_owned());
    let json = serde_json::to_string(&SchemaRef::Inline(Box::new(original))).unwrap();
    let back: SchemaRef = serde_json::from_str(&json).unwrap();
    match back {
        SchemaRef::Inline(s) => {
            assert!(s.ref_path.is_none());
            assert_eq!(s.any_of.as_ref().map(Vec::len), Some(2));
        }
        SchemaRef::Ref(_) => panic!("a nullable reference must round-trip as inline"),
    }
}

#[test]
fn schema_ref_inline_schema_preserves_every_supported_keyword() {
    let input = serde_json::json!({
        "$ref": "#/components/schemas/Base",
        "type": "object",
        "format": "custom",
        "title": "Complete schema",
        "description": "Every supported keyword",
        "default": {"enabled": true},
        "example": {"id": 1},
        "examples": [{"id": 2}],
        "minimum": 0,
        "maximum": 10.5,
        "exclusiveMinimum": 1,
        "exclusiveMaximum": 9.5,
        "multipleOf": 2,
        "minLength": 1,
        "maxLength": 20,
        "pattern": "^[a-z]+$",
        "items": {"$ref": "#/components/schemas/Item"},
        "prefixItems": [{"type": "string"}],
        "minItems": 1,
        "maxItems": 5,
        "uniqueItems": true,
        "properties": {"id": {"type": "integer"}},
        "required": ["id"],
        "additionalProperties": false,
        "minProperties": 1,
        "maxProperties": 3,
        "enum": [{"id": 1}, {"id": 2}],
        "allOf": [{"$ref": "#/components/schemas/All"}],
        "anyOf": [{"type": "string"}],
        "oneOf": [{"type": "integer"}],
        "not": {"type": "boolean"},
        "discriminator": {
            "propertyName": "kind",
            "mapping": {"entry": "#/components/schemas/Entry"}
        },
        "nullable": false,
        "readOnly": true,
        "writeOnly": false,
        "externalDocs": {"description": "Reference", "url": "https://example.com"},
        "$defs": {"Local": {"type": "number"}},
        "$dynamicAnchor": "node",
        "$dynamicRef": "#node"
    });

    let schema_ref: SchemaRef = serde_json::from_value(input).unwrap();
    let SchemaRef::Inline(schema) = schema_ref else {
        panic!("a $ref with sibling keywords must deserialize as an inline schema");
    };
    assert_eq!(
        schema.ref_path.as_deref(),
        Some("#/components/schemas/Base")
    );
    assert_eq!(schema.schema_type, Some(SchemaType::Object));
    assert_eq!(schema.nullable, Some(false));

    let output = serde_json::to_value(schema).unwrap();
    assert_eq!(output["format"], "custom");
    assert_eq!(output["title"], "Complete schema");
    assert_eq!(output["description"], "Every supported keyword");
    assert_eq!(output["default"], serde_json::json!({"enabled": true}));
    assert_eq!(
        output["examples"],
        serde_json::json!([{"id": 1}, {"id": 2}])
    );
    assert_eq!(output["minimum"], 0);
    assert_eq!(output["maximum"], 10.5);
    assert_eq!(output["exclusiveMinimum"], 1);
    assert_eq!(output["exclusiveMaximum"], 9.5);
    assert_eq!(output["multipleOf"], 2);
    assert_eq!(output["minLength"], 1);
    assert_eq!(output["maxLength"], 20);
    assert_eq!(output["pattern"], "^[a-z]+$");
    assert_eq!(output["items"]["$ref"], "#/components/schemas/Item");
    assert_eq!(output["prefixItems"][0]["type"], "string");
    assert_eq!(output["minItems"], 1);
    assert_eq!(output["maxItems"], 5);
    assert_eq!(output["uniqueItems"], true);
    assert_eq!(output["properties"]["id"]["type"], "integer");
    assert_eq!(output["required"], serde_json::json!(["id"]));
    assert_eq!(output["additionalProperties"], false);
    assert_eq!(output["minProperties"], 1);
    assert_eq!(output["maxProperties"], 3);
    assert_eq!(output["enum"], serde_json::json!([{"id": 1}, {"id": 2}]));
    assert_eq!(output["allOf"][0]["$ref"], "#/components/schemas/All");
    assert_eq!(output["anyOf"][0]["type"], "string");
    assert_eq!(output["oneOf"][0]["type"], "integer");
    assert_eq!(output["not"]["type"], "boolean");
    assert_eq!(output["discriminator"]["propertyName"], "kind");
    assert_eq!(output["readOnly"], true);
    assert_eq!(output["writeOnly"], false);
    assert_eq!(output["externalDocs"]["url"], "https://example.com");
    assert_eq!(output["$defs"]["Local"]["type"], "number");
    assert_eq!(output["$dynamicAnchor"], "node");
    assert_eq!(output["$dynamicRef"], "#node");
}
