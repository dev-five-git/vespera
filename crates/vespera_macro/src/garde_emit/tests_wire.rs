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
    "Some (< _ as :: vespera :: __validation :: RangeBound > :: exact (3))"
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
/// compile error, not a `quote!` panic.
#[rstest]
#[case::float_field("#[schema(maximum = 1e400)] f: f64")]
#[case::unknown_type("#[schema(minimum = -1e400)] f: Count")]
fn non_finite_bound_is_a_compile_error(#[case] field: &str) {
    let out = emit(&format!("struct S {{ {field} }}"));
    assert!(out.contains("compile_error"), "{out}");
    assert!(out.contains("must be a finite number"), "{out}");
}
