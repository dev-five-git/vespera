//! Container-level `#[schema(value_type = T)]` takes a TYPE (not an
//! expression) and drives the derive's leaf-type assertions.

use rstest::rstest;

use super::*;

#[rstest]
#[case::generic_type(
    "#[schema(value_type = Vec<Wire>)] struct S { a: Opaque }",
    true,
    false
)]
#[case::with_name(
    "#[schema(name = \"Renamed\", value_type = Option<Wire>)] struct S { a: Opaque }",
    true,
    false
)]
#[case::serde_into(r#"#[serde(into = "Wire")] struct S { a: Opaque }"#, true, true)]
fn container_value_type_drives_assertions(
    #[case] src: &str,
    #[case] asserts_wire: bool,
    #[case] asserts_fields: bool,
) {
    let input: syn::DeriveInput = syn::parse_str(src).unwrap();
    let (metadata, expanded) = process_derive_schema(&input);
    assert!(metadata.is_some(), "derive must succeed: {expanded}");
    let expanded = expanded.to_string();
    assert_eq!(
        expanded.contains("__vespera_assert_schema :: < Wire >"),
        asserts_wire
    );
    assert_eq!(
        expanded.contains("__vespera_assert_schema :: < Opaque >"),
        asserts_fields
    );
}

#[test]
fn container_value_type_must_be_a_type() {
    let input: syn::DeriveInput =
        syn::parse_str("#[schema(value_type = 1 + 2)] struct S { a: u8 }").unwrap();
    let (metadata, expanded) = process_derive_schema(&input);
    assert!(metadata.is_none());
    assert!(expanded.to_string().contains("compile_error"), "{expanded}");
}

/// A malformed recognized `#[schema(..)]` key is a compile error wherever
/// the schema parser reads one — not only on named struct fields, where it
/// would otherwise silently drop the whole attribute's constraints.
#[rstest]
#[case::container(
    r#"#[schema(name = "X", example = foo())] #[schema(value_type = u8)] struct S { a: u8 }"#
)]
#[case::tuple_struct(r#"struct S(#[schema(min_length = "3")] String);"#)]
#[case::enum_named_field(
    r#"enum E { A { #[schema(min_length = "3", max_length = 10)] s: String } }"#
)]
#[case::enum_tuple_field(r#"enum E { A(#[schema(maximum = "x")] u8) }"#)]
fn malformed_schema_constraints_are_compile_errors(#[case] src: &str) {
    let input: syn::DeriveInput = syn::parse_str(src).unwrap();
    let (metadata, expanded) = process_derive_schema(&input);
    assert!(metadata.is_none(), "{src}");
    assert!(expanded.to_string().contains("compile_error"), "{expanded}");
}

#[test]
fn well_formed_positional_constraints_derive() {
    let input: syn::DeriveInput = syn::parse_str(
        "enum E { A(#[schema(maximum = 10)] u8), B { #[schema(min_length = 1)] s: String } }",
    )
    .unwrap();
    assert!(process_derive_schema(&input).0.is_some());
}
