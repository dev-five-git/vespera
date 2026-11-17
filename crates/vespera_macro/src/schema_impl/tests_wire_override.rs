//! Container-level `#[schema(value_type = T)]` takes a TYPE (not an
//! expression) and drives the derive's leaf-type assertions.

use rstest::rstest;

use super::*;

/// A custom `#[schema(name)]` / `schema_type!(name = ..)` is a
/// `components.schemas` key: the OpenAPI pattern, checked at compile time.
#[rstest]
#[case::plain("UserSchema", true)]
#[case::dotted_dashed("user.v2-dto_x", true)]
#[case::space("user dto", false)]
#[case::slash("a/b", false)]
#[case::empty("", false)]
#[case::non_ascii("회원", false)]
fn schema_name_must_be_a_component_key(#[case] name: &str, #[case] valid: bool) {
    let result = require_component_key(name, proc_macro2::Span::call_site());
    assert_eq!(result.is_ok(), valid, "{name}");
}

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
// A custom name outside the component-key pattern.
#[case::invalid_schema_name(r#"#[schema(name = "bad name")] struct S { a: u8 }"#)]
fn malformed_schema_constraints_are_compile_errors(#[case] src: &str) {
    let input: syn::DeriveInput = syn::parse_str(src).unwrap();
    let (metadata, expanded) = process_derive_schema(&input);
    assert!(metadata.is_none(), "{src}");
    assert!(expanded.to_string().contains("compile_error"), "{expanded}");
}

/// A custom serde codec with no well-known wire shape, no string type and no
/// `value_type` is a compile error on the field (struct fields and enum
/// variant fields alike).
#[rstest]
#[case::struct_field(r#"struct S { #[serde(with = "hex")] checksum: Vec<u8> }"#)]
#[case::enum_variant_field(r#"enum E { A(#[serde(deserialize_with = "f")] u8) }"#)]
fn unknown_codec_fields_are_compile_errors(#[case] src: &str) {
    let input: syn::DeriveInput = syn::parse_str(src).unwrap();
    let (metadata, expanded) = process_derive_schema(&input);
    assert!(metadata.is_none(), "{src}");
    assert!(expanded.to_string().contains("compile_error"), "{expanded}");
}

#[test]
fn value_type_overrides_an_unknown_codec() {
    let input: syn::DeriveInput = syn::parse_str(
        r#"struct S { #[serde(with = "hex")] #[schema(value_type = String)] checksum: Vec<u8> }"#,
    )
    .unwrap();
    assert!(process_derive_schema(&input).0.is_some());
}

#[test]
fn well_formed_positional_constraints_derive() {
    let input: syn::DeriveInput = syn::parse_str(
        "enum E { A(#[schema(maximum = 10)] u8), B { #[schema(min_length = 1)] s: String } }",
    )
    .unwrap();
    assert!(process_derive_schema(&input).0.is_some());
}
