//! Deserialization of `Schema` in every position: boolean schemas and the
//! OpenAPI 3.0 boolean `exclusiveMinimum` / `exclusiveMaximum`.

use super::*;
use rstest::rstest;
use serde_json::json;

#[test]
fn boolean_schemas_deserialize_as_their_object_equivalents() {
    let accept_all: Schema = serde_json::from_value(json!(true)).unwrap();
    assert_eq!(serde_json::to_value(&accept_all).unwrap(), json!({}));

    let reject_all: Schema = serde_json::from_value(json!(false)).unwrap();
    assert_eq!(
        serde_json::to_value(&reject_all).unwrap(),
        json!({"not": {}})
    );
}

#[test]
fn pure_ref_object_deserializes_into_ref_path() {
    let schema: Schema =
        serde_json::from_value(json!({"$ref": "#/components/schemas/User"})).unwrap();
    assert_eq!(
        schema.ref_path.as_deref(),
        Some("#/components/schemas/User")
    );
    assert_eq!(
        serde_json::to_value(&schema).unwrap(),
        json!({"$ref": "#/components/schemas/User"})
    );
}

#[test]
fn boolean_schemas_round_trip_in_components_and_defs() {
    let components: Components = serde_json::from_value(json!({
        "schemas": {
            "Anything": true,
            "Nothing": false,
            "Holder": {"type": "object", "$defs": {"Any": true, "None": false}},
        }
    }))
    .unwrap();

    assert_eq!(
        serde_json::to_value(&components).unwrap(),
        json!({
            "schemas": {
                "Anything": {},
                "Nothing": {"not": {}},
                "Holder": {"type": "object", "$defs": {"Any": {}, "None": {"not": {}}}},
            }
        })
    );
}

#[rstest]
#[case::flag_true_moves_minimum(json!({"minimum": 5, "exclusiveMinimum": true}), None, Some(5.0))]
#[case::flag_before_minimum(json!({"exclusiveMinimum": true, "minimum": 5}), None, Some(5.0))]
#[case::flag_false_keeps_minimum(json!({"minimum": 5, "exclusiveMinimum": false}), Some(5.0), None)]
#[case::flag_without_minimum(json!({"exclusiveMinimum": true}), None, None)]
#[case::unsigned_number(json!({"exclusiveMinimum": 3}), None, Some(3.0))]
#[case::negative_number(json!({"exclusiveMinimum": -3}), None, Some(-3.0))]
#[case::float_number(json!({"minimum": 1, "exclusiveMinimum": 1.5}), Some(1.0), Some(1.5))]
fn exclusive_minimum_accepts_openapi_30_flag_and_31_number(
    #[case] input: serde_json::Value,
    #[case] minimum: Option<f64>,
    #[case] exclusive_minimum: Option<f64>,
) {
    let schema: Schema = serde_json::from_value(input).unwrap();
    assert_eq!(schema.minimum, minimum);
    assert_eq!(schema.exclusive_minimum, exclusive_minimum);
}

#[rstest]
#[case::flag_true_moves_maximum(json!({"maximum": 9, "exclusiveMaximum": true}), None, Some(9.0))]
#[case::flag_false_keeps_maximum(json!({"maximum": 9, "exclusiveMaximum": false}), Some(9.0), None)]
#[case::number(json!({"exclusiveMaximum": 9}), None, Some(9.0))]
fn exclusive_maximum_accepts_openapi_30_flag_and_31_number(
    #[case] input: serde_json::Value,
    #[case] maximum: Option<f64>,
    #[case] exclusive_maximum: Option<f64>,
) {
    let schema: Schema = serde_json::from_value(input).unwrap();
    assert_eq!(schema.maximum, maximum);
    assert_eq!(schema.exclusive_maximum, exclusive_maximum);
}

#[test]
fn openapi_30_exclusive_flag_serializes_in_31_form() {
    let schema: SchemaRef =
        serde_json::from_value(json!({"type": "integer", "minimum": 0, "exclusiveMinimum": true}))
            .unwrap();
    assert_eq!(
        serde_json::to_value(&schema).unwrap(),
        json!({"type": "integer", "exclusiveMinimum": 0})
    );
}

#[test]
fn exclusive_bound_rejects_a_non_number_non_boolean() {
    let error = serde_json::from_value::<Schema>(json!({"exclusiveMinimum": "5"})).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a number or (OpenAPI 3.0) a boolean"),
        "unexpected error: {error}"
    );
}

/// Round 11: a `"null"` in the `type` array must not widen a schema whose
/// `enum` / composition rejects `null` — re-serializing it used to append
/// `null` to the enum or wrap it in `anyOf [.., {type: null}]`.
#[rstest]
#[case::enum_without_null(
    json!({"type": ["string", "null"], "enum": ["a", "b"]}),
    json!({"type": "string", "enum": ["a", "b"]})
)]
#[case::all_of(
    json!({"type": ["object", "null"], "allOf": [{"$ref": "#/components/schemas/A"}]}),
    json!({"type": "object", "allOf": [{"$ref": "#/components/schemas/A"}]})
)]
#[case::enum_with_null(
    json!({"type": ["string", "null"], "enum": ["a", null]}),
    json!({"type": ["string", "null"], "enum": ["a", null]})
)]
#[case::any_of_with_null_branch(
    json!({"type": ["object", "null"], "anyOf": [{"type": "object"}, {"type": "null"}]}),
    json!({"type": ["object", "null"], "anyOf": [{"type": "object"}, {"type": "null"}]})
)]
#[case::plain(json!({"type": ["integer", "null"]}), json!({"type": ["integer", "null"]}))]
fn type_array_null_never_widens_a_schema_that_rejects_null(
    #[case] input: serde_json::Value,
    #[case] expected: serde_json::Value,
) {
    let schema: Schema = serde_json::from_value(input).unwrap();
    assert_eq!(serde_json::to_value(&schema).unwrap(), expected);
}

/// Round 12: a `"null"` in the `type` array next to a `oneOf` / `not` /
/// `allOf` that admits `null` is kept (as `nullable`, re-serialized through
/// the `anyOf [.., {type: null}]` wrapper, the same accepted set); it used
/// to be dropped for every `oneOf` / `allOf` / `not`, narrowing the schema.
#[rstest]
#[case::one_of_with_null_branch(
    json!({"type": ["object", "null"], "oneOf": [{"$ref": "#/components/schemas/A"}, {"type": "null"}]}),
    Some(true)
)]
#[case::one_of_without_null_branch(
    json!({"type": ["object", "null"], "oneOf": [{"$ref": "#/components/schemas/A"}]}),
    None
)]
#[case::not_of_non_null_type(json!({"type": ["string", "null"], "not": {"type": "integer"}}), Some(true))]
#[case::not_of_enum_without_null(json!({"type": ["string", "null"], "not": {"enum": ["x"]}}), Some(true))]
#[case::not_of_enum_with_null(json!({"type": ["string", "null"], "not": {"enum": ["x", null]}}), None)]
#[case::not_of_nullable_type(
    json!({"type": ["string", "null"], "not": {"type": ["integer", "null"]}}),
    None
)]
#[case::not_of_null_type(json!({"type": ["string", "null"], "not": {"type": "null"}}), None)]
#[case::not_of_ref(
    json!({"type": ["string", "null"], "not": {"$ref": "#/components/schemas/B"}}),
    None
)]
#[case::not_of_empty(json!({"type": ["string", "null"], "not": {}}), None)]
#[case::all_of_only_null(json!({"type": ["object", "null"], "allOf": [{"type": "null"}]}), Some(true))]
#[case::all_of_mixed(
    json!({"type": ["object", "null"], "allOf": [{"type": "null"}, {"$ref": "#/components/schemas/A"}]}),
    None
)]
// Round 14: a member admits `null` when it is nullable or type-less, not only
// when it is exactly `{type: null}`.
#[case::all_of_nullable_member(
    json!({"type": ["integer", "null"], "allOf": [{"type": ["integer", "null"]}]}),
    Some(true)
)]
#[case::all_of_typeless_member(json!({"type": ["integer", "null"], "allOf": [{"description": "d"}]}), Some(true))]
#[case::all_of_typeless_enum_without_null(json!({"type": ["string", "null"], "allOf": [{"enum": ["a"]}]}), None)]
#[case::all_of_typeless_rejecting_composition(
    json!({"type": ["string", "null"], "allOf": [{"allOf": [{"$ref": "#/components/schemas/A"}]}]}),
    None
)]
#[case::any_of_nullable_member(
    json!({"type": ["string", "null"], "anyOf": [{"type": ["string", "null"]}]}),
    Some(true)
)]
#[case::any_of_empty_member(json!({"type": ["string", "null"], "anyOf": [{"type": "integer"}, {}]}), Some(true))]
#[case::one_of_nullable_member(
    json!({"type": ["string", "null"], "oneOf": [{"type": ["string", "null"]}, {"type": "integer"}]}),
    Some(true)
)]
#[case::one_of_two_null_branches(
    json!({"type": ["string", "null"], "oneOf": [{"type": "null"}, {"type": ["string", "null"]}]}),
    None
)]
fn type_array_null_kept_when_the_composition_admits_null(
    #[case] input: serde_json::Value,
    #[case] nullable: Option<bool>,
) {
    let schema: Schema = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(schema.nullable, nullable, "{input}");
    let output = serde_json::to_value(&schema).unwrap();
    if nullable == Some(true) {
        // The wrapper's `{type: null}` branch keeps `null` accepted.
        assert_eq!(output["anyOf"][1], json!({"type": "null"}), "{output}");
    } else {
        assert!(output.get("anyOf").is_none(), "{output}");
    }
}

/// Round 13: a `$ref` next to `type: [T, "null"]` (or an explicit 3.0
/// `nullable` next to `$ref` plus `type` / `anyOf`) used to deserialize into
/// a nullable `$ref` with a sibling `type` / `anyOf`, which serialization
/// rejects — the loaded document could not be written back.  The type
/// array's `null` is dropped (the `$ref` is ANDed with it, like `allOf`);
/// the explicit `nullable` keeps widening through `anyOf [.., {type: null}]`.
#[rstest]
#[case::ref_with_type_array_null(
    json!({"$ref": "#/components/schemas/User", "type": ["object", "null"]}),
    json!({"$ref": "#/components/schemas/User", "type": "object"})
)]
#[case::type_array_null_before_ref(
    json!({"type": ["object", "null"], "$ref": "#/components/schemas/User"}),
    json!({"$ref": "#/components/schemas/User", "type": "object"})
)]
#[case::explicit_nullable_with_type(
    json!({"$ref": "#/components/schemas/User", "nullable": true, "type": "object"}),
    json!({"anyOf": [{"$ref": "#/components/schemas/User", "type": "object"}, {"type": "null"}]})
)]
#[case::explicit_nullable_with_any_of(
    json!({"$ref": "#/components/schemas/User", "nullable": true, "anyOf": [{"type": "object"}, {"type": "null"}]}),
    json!({"anyOf": [
        {"$ref": "#/components/schemas/User", "anyOf": [{"type": "object"}, {"type": "null"}]},
        {"type": "null"}
    ]})
)]
#[case::explicit_nullable_ref_only(
    json!({"$ref": "#/components/schemas/User", "nullable": true}),
    json!({"anyOf": [{"$ref": "#/components/schemas/User"}, {"type": "null"}]})
)]
fn ref_with_sibling_type_or_any_of_round_trips(
    #[case] input: serde_json::Value,
    #[case] expected: serde_json::Value,
) {
    let schema: Schema = serde_json::from_value(input).unwrap();
    let output = serde_json::to_value(&schema).unwrap();
    assert_eq!(output, expected);
    // The output is a fixed point: it reads back into the same document.
    let again: Schema = serde_json::from_value(output.clone()).unwrap();
    assert_eq!(serde_json::to_value(&again).unwrap(), output);
}
