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
