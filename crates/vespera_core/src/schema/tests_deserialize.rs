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
