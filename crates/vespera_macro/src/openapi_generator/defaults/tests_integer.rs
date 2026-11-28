//! `#[schema(default = "...")]` literals of an integer-typed property (round-44
//! R44-MR3-L1): a literal that denotes an integer exactly is that JSON integer,
//! never the `f64` the general parse gives (`+5` used to read `5.0`).

use rstest::rstest;
use serde_json::{Value, json};
use vespera_core::schema::{Schema, SchemaRef};

use super::*;

fn integer_property() -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema::integer()))
}

#[rstest]
#[case::plus_sign("+5", json!(5))]
#[case::minus_zero("-0", json!(0))]
#[case::plain("42", json!(42))]
#[case::exponent_whole("1e3", json!(1000))]
#[case::upper_exponent("2E2", json!(200))]
#[case::signed_exponent("+1e+2", json!(100))]
#[case::zero_fraction("5.0", json!(5))]
#[case::zero_fraction_exponent("1.5e1", json!(15))]
#[case::negative_exponent_zero_digits("-0.00", json!(0))]
#[case::i64_min("-9223372036854775808", json!(i64::MIN))]
#[case::u64_max("18446744073709551615", json!(u64::MAX))]
#[case::u64_max_exponent("1.8446744073709551615e19", json!(u64::MAX))]
fn exact_integer_literal_is_an_integer(#[case] literal: &str, #[case] expected: Value) {
    let value = schema_default_value(Some(&integer_property()), literal.to_string());
    assert_eq!(value, expected);
    assert!(value.is_i64() || value.is_u64(), "{value}");
}

#[rstest]
#[case::fraction("1.5")]
#[case::negative_exponent("1e-3")]
#[case::not_a_number("nan")]
#[case::empty_exponent("1e")]
#[case::fraction_past_the_exponent("1.00000000000000000001e3")]
#[case::beyond_u64("18446744073709551616")]
#[case::below_i64("-9223372036854775809")]
#[case::huge_exponent("1e400")]
fn literal_that_is_not_an_exact_integer_keeps_the_general_parse(#[case] literal: &str) {
    let expected = parse_default_string_to_json_value(literal);
    assert_eq!(
        schema_default_value(Some(&integer_property()), literal.to_string()),
        expected
    );
}

#[test]
fn fractional_literal_stays_a_float() {
    let value = schema_default_value(Some(&integer_property()), "1.5".to_string());
    assert_eq!(value, json!(1.5));
    assert!(value.is_f64(), "{value}");
}

#[test]
fn null_literal_on_an_integer_property_is_null() {
    assert_eq!(
        schema_default_value(Some(&integer_property()), "null".to_string()),
        Value::Null
    );
}

#[test]
fn exact_integer_literal_rejects_malformed_text() {
    for literal in ["", "+", "-", ".", "e5", "1..2", "1e5e5", "1.2.3", "1 0"] {
        assert!(exact_integer_literal(literal).is_none(), "{literal}");
    }
}
