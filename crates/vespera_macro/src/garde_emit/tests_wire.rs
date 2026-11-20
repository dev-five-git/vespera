//! Validation follows the wire: skipped / flattened fields, every `Option`
//! layer, exact bounds read like the parsed constraints, and the documented
//! (`f64`) value of a bound.
use rstest::rstest;

use super::*;

fn emit(src: &str) -> String {
    let input: DeriveInput = syn::parse_str(src).unwrap();
    let Data::Struct(data) = &input.data else {
        unreachable!()
    };
    let constraints: Vec<SchemaConstraints> = data
        .fields
        .iter()
        .map(|field| {
            crate::parser::schema::schema_attrs::try_extract_schema_constraints(&field.attrs)
                .unwrap()
        })
        .collect();
    emit_garde_validate(&input, &constraints).to_string()
}

/// A skipped field gets no block (serde fills its default); a flattened one
/// keeps the parent's path (no `nested_path!` of its own).
#[test]
fn skipped_fields_are_not_validated_and_flatten_keeps_the_path() {
    let out = emit(
        "struct S { #[serde(skip)] #[schema(min_length = 1)] a: String, \
         #[serde(skip_deserializing)] #[schema(minimum = 1)] b: u32, \
         #[serde(flatten)] #[schema(dive)] c: Page, \
         #[schema(min_length = 1)] d: String }",
    );
    assert_eq!(out.matches("nested_path").count(), 1, "{out}");
    assert!(out.contains("\"d\""), "{out}");
    assert!(
        !out.contains("\"a\"") && !out.contains("\"b\"") && !out.contains("\"c\""),
        "{out}"
    );
    assert_eq!(
        out.matches("validate_into (& * __garde_binding").count(),
        1,
        "{out}"
    );
}

/// A struct whose only runtime rules sit on skipped fields still gets the
/// (empty) `Validate` impl: `Validated<Json<S>>` requires it, and dropping it
/// broke compiling payloads that had compiled before.
#[test]
fn skipped_runtime_rule_only_struct_emits_an_empty_impl() {
    let out = emit("struct S { #[serde(skip)] #[schema(min_length = 1)] a: String }");
    assert!(
        out.contains("impl :: vespera :: __validation :: garde :: Validate for S"),
        "{out}"
    );
    assert!(!out.contains("\"a\""), "{out}");
}

/// A direct `f32` bound rounds inward (round-27 coordinator item): the nearest
/// `f32` to `16777217` is `16777216`, which would admit a value the bound
/// forbids, so the minimum steps up and the maximum steps down. `f64` keeps
/// the bound as written.
#[test]
fn f32_bounds_round_inward_and_f64_bounds_do_not() {
    let min = emit("struct S { #[schema(minimum = 16777217)] f: f32 }");
    assert!(min.contains("next_up"), "{min}");
    assert!(!min.contains("next_down"), "{min}");
    let max = emit("struct S { #[schema(maximum = 16777217)] f: f32 }");
    assert!(max.contains("next_down"), "{max}");
    assert!(!max.contains("next_up"), "{max}");
    let f64_min = emit("struct S { #[schema(minimum = 1.5)] f: f64 }");
    assert!(!f64_min.contains("next_up"), "{f64_min}");
    assert!(f64_min.contains("1.5f64 as f64"), "{f64_min}");
}

/// A direct `f32` bound whose shortest decimal is the written value keeps the
/// nearest `f32` (round-27 regression): `0.1` must stay `0.1f32`, not step to
/// the neighbouring `f32` (which would reject the advertised minimum).
#[test]
fn f32_bound_with_a_round_tripping_decimal_is_kept_exactly() {
    let max = emit("struct S { #[schema(maximum = 0.1)] f: f32 }");
    assert!(max.contains("0.1f32"), "{max}");
    assert!(!max.contains("next_down"), "{max}");
    assert!(!max.contains("as f32"), "{max}");
}

/// Maximum and minimum bounds that round to `u64::MAX` / `i64::MAX`'s float
/// image (2^64 / 2^63) are the integer maximum only for a MAXIMUM; a MINIMUM
/// there admits no value of the type and is a "does not fit" error.
#[rstest]
#[case::u64_max_at_2_pow_64(18_446_744_073_709_551_616.0, Bound::Max, "u64", true)]
#[case::u64_min_at_2_pow_64(18_446_744_073_709_551_616.0, Bound::Min, "u64", false)]
#[case::i64_min_at_2_pow_63(9_223_372_036_854_775_808.0, Bound::Min, "i64", false)]
#[case::i64_max_at_2_pow_63(9_223_372_036_854_775_808.0, Bound::Max, "i64", true)]
#[case::u128_max_at_2_pow_128(
    340_282_366_920_938_463_463_374_607_431_768_211_456.0,
    Bound::Max,
    "u128",
    true
)]
#[case::u128_min_at_2_pow_128(
    340_282_366_920_938_463_463_374_607_431_768_211_456.0,
    Bound::Min,
    "u128",
    false
)]
fn float_bound_at_the_integer_edge_is_max_only(
    #[case] value: f64,
    #[case] bound: Bound,
    #[case] kind: &str,
    #[case] accepted: bool,
) {
    let out = numeric_some(Some(value), None, bound, Some(kind));
    assert_eq!(out.is_ok(), accepted, "{out:?}");
    if let Ok(tokens) = out {
        assert!(tokens.to_string().contains(kind), "{tokens}");
    }
}

/// A generic struct whose only runtime rule sits on a skipped field gets the
/// empty impl with its generics (round-27 macrorest-1 L2), not the
/// "generic types" compile error: serde never reads that field.
#[test]
fn skipped_runtime_rule_on_generic_struct_emits_an_empty_generic_impl() {
    let out = emit("struct W<T> { #[serde(skip)] #[schema(minimum = 1)] n: i32, v: T }");
    assert!(!out.contains("compile_error"), "{out}");
    assert!(
        out.contains("impl < T > :: vespera :: __validation :: garde :: Validate for W < T >"),
        "{out}"
    );
}

/// A struct with no runtime rule at all gets no impl.
#[test]
fn struct_without_runtime_rules_emits_no_impl() {
    let out = emit("struct S { #[schema(description = \"x\")] a: String }");
    assert!(out.is_empty(), "{out}");
}

/// Every `Option` layer is peeled before its rule runs: `Option<Option<i32>>`
/// is checked on the inner `i32`, so the rule never sees an `Option`.
#[test]
fn nested_option_peels_every_layer_before_the_rule() {
    let out = emit("struct S { #[schema(minimum = 0.5)] f: Option<Option<i32>> }");
    assert_eq!(out.matches("= __garde_binding {").count(), 1, "{out}");
    assert!(out.contains("Some (1i32)"), "{out}");
}

/// A `#[serde(transparent)]` struct's field is the struct itself on the
/// wire: its rules keep the parent's path.
#[test]
fn transparent_struct_field_keeps_the_parent_path() {
    let out =
        emit("#[serde(transparent)] struct Email { #[schema(format = \"email\")] value: String }");
    assert_eq!(out.matches("nested_path").count(), 0, "{out}");
    assert!(!out.contains("\"value\""), "{out}");
}

/// The rendered `(min, max)` range bounds of one field.
#[rstest]
#[case::nested_option("#[schema(minimum = 0.5)] f: Option<Option<i32>>", "Some (1i32)")]
#[case::alias_exact(
    "#[schema(maximum = 3)] f: Count",
    "Some (< _ as :: vespera :: __validation :: RangeBound > :: exact_within (3 , 3f64 , false))"
)]
#[case::alias_past_2_pow_53(
    "#[schema(minimum = 9007199254740993)] f: UserId",
    "Some (< _ as :: vespera :: __validation :: RangeBound > :: exact_within (9007199254740993 , 9007199254740992f64 , true))"
)]
#[case::alias_unreachable_guard(
    "#[schema(minimum = 300.5, maximum = 3)] f: Small",
    "range_unsatisfiable (__garde_binding , :: std :: option :: Option :: Some (300.5f64) , :: std :: option :: Option :: None)"
)]
#[case::alias_fraction(
    "#[schema(maximum = 2.5)] f: Count",
    "Some (< _ as :: vespera :: __validation :: RangeBound > :: max (2.5f64))"
)]
#[case::alias_fraction_min(
    "#[schema(minimum = 0.5)] f: Count",
    "Some (< _ as :: vespera :: __validation :: RangeBound > :: min (0.5f64))"
)]
#[case::past_2_pow_53(
    "#[schema(minimum = 9007199254740993)] f: i64",
    "Some (9007199254740992i64)"
)]
#[case::negative_min_keeps_exact(
    "#[schema(minimum = -9007199254740993)] f: i64",
    "Some (- 9007199254740993i64)"
)]
#[case::negative_max_loosens(
    "#[schema(maximum = -9007199254740993)] f: i64",
    "Some (- 9007199254740992i64)"
)]
#[case::i64_max_clamped(
    "#[schema(maximum = 9223372036854775807)] f: i64",
    "Some (9223372036854775807i64)"
)]
#[case::i64_min(
    "#[schema(minimum = -9223372036854775808)] f: i64",
    "Some (- 9223372036854775808i64)"
)]
#[case::u64_max_clamped(
    "#[schema(maximum = 18446744073709551615)] f: u64",
    "Some (18446744073709551615u64)"
)]
#[case::u64_float_max_is_the_integer_max(
    "#[schema(maximum = 18446744073709551615.0)] f: u64",
    "Some (18446744073709551615u64)"
)]
#[case::i64_float_max_is_the_integer_max(
    "#[schema(maximum = 9223372036854775807.0)] f: i64",
    "Some (9223372036854775807i64)"
)]
#[case::value_type_attr_not_stale(
    "#[schema(minimum = 9007199254740993)] #[schema(value_type = Vec<u8>, minimum = 3)] f: i64",
    "Some (3i64)"
)]
#[case::last_attr_wins(
    "#[schema(minimum = 9007199254740993)] #[schema(minimum = 2.5)] f: i64",
    "Some (3i64)"
)]
#[case::group_and_flag(
    "#[schema(dive, foo(bar), name = \"x\", minimum = 7)] f: u8",
    "Some (7u8)"
)]
fn range_bounds_follow_the_document(#[case] field: &str, #[case] expected: &str) {
    let out = emit(&format!("struct S {{ {field} }}"));
    assert!(out.contains(expected), "{out}");
}

/// A bound `f64` cannot hold (`1e400` parses to infinity) is a spanned
/// error of the `#[schema]` parser, so no validator is ever emitted for it.
#[rstest]
#[case::float_field("#[schema(maximum = 1e400)] f: f64")]
#[case::unknown_type("#[schema(minimum = -1e400)] f: Count")]
fn non_finite_bound_is_a_compile_error(#[case] field: &str) {
    let item: syn::ItemStruct = syn::parse_str(&format!("struct S {{ {field} }}")).unwrap();
    let attrs = &item.fields.iter().next().unwrap().attrs;
    let err = crate::parser::schema::schema_attrs::try_extract_schema_constraints(attrs)
        .expect_err("non-finite bound");
    assert!(err.to_string().contains("must be a finite number"), "{err}");
}

/// An integer literal wider than `i128` (the literal an alias of `f64` takes)
/// is not emitted as an `i128` literal, which rustc rejects as out of range:
/// the documented `f64` bound goes through `RangeBound::min` / `max` instead.
#[rstest]
#[case::positive_min(
    "#[schema(minimum = 200000000000000000000000000000000000000)] f: Ratio",
    "min"
)]
#[case::negative_max(
    "#[schema(maximum = -200000000000000000000000000000000000000)] f: Ratio",
    "max"
)]
fn literal_wider_than_i128_on_an_alias_uses_the_f64_path(#[case] field: &str, #[case] call: &str) {
    let out = emit(&format!("struct S {{ {field} }}"));
    assert!(out.contains(&format!("RangeBound > :: {call} (")), "{out}");
    assert!(!out.contains("exact_within"), "{out}");
}
