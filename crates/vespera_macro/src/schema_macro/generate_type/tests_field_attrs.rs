//! Carried field attributes (`#[schema]`, `#[garde]`, `#[deprecated]`,
//! `#[cfg]`, `#[cfg_attr]`) and the `ignore` / metadata split.

use std::collections::HashMap;

use rstest::rstest;

use super::*;
use crate::schema_macro::SchemaTypeInput;

const SOURCE: &str = r#"
#[garde(context(Ctx))]
pub struct Source {
    #[schema(minimum = 1)]
    #[cfg(feature = "x")]
    pub gated: i32,
    #[garde(length(min = 1))]
    #[deprecated]
    pub title: String,
    #[sea_orm(default_value = "5")]
    pub seeded: i32,
    #[sea_orm(default_value = "7")]
    #[schema(default = 9)]
    pub overridden: i32,
    #[cfg_attr(feature = "x", schema(maximum = 3), sea_orm(indexed))]
    pub plain: bool,
}
"#;

fn generate(input: &str) -> (String, Option<String>) {
    let storage = HashMap::from([(
        "Source".to_string(),
        StructMetadata::new("Source".to_string(), SOURCE.to_string()),
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let (tokens, metadata) = generate_schema_type_code(&input, &storage).unwrap();
    let file: syn::File = syn::parse2(tokens).unwrap();
    (
        prettyplease::unparse(&file),
        metadata.map(|metadata| {
            let file: syn::File = syn::parse_str(&metadata.definition).unwrap();
            prettyplease::unparse(&file)
        }),
    )
}

#[rstest]
#[case::derived("field_attrs_derived", "Dto from Source")]
#[case::with_add("field_attrs_with_add", r#"Dto from Source, add = [("extra": u8)]"#)]
#[case::ignored_named(
    "field_attrs_ignored_named",
    r#"Dto from Source, ignore, name = "DtoSchema""#
)]
#[case::multipart("field_attrs_multipart", "Dto from Source, multipart")]
fn carried_field_attributes(#[case] snapshot: &str, #[case] input: &str) {
    let (code, metadata) = generate(input);
    insta::assert_snapshot!(
        snapshot,
        format!(
            "{code}\n// ---- metadata ----\n{}",
            metadata.unwrap_or_default()
        )
    );
}

const DOCUMENTED: &str = r"
/// An upload.
#[serde(deny_unknown_fields, default)]
#[try_from_multipart(strict)]
pub struct Documented {
    pub title: String,
}
";

/// The metadata an `ignore, name = ".."` DTO registers is the DTO the
/// macro emits: it keeps the struct docs (the schema description) and, in
/// `multipart` mode, the multipart container attributes instead of the
/// serde-mode ones.
#[rstest]
#[case::serde(
    "ignored_metadata_serde",
    r#"Dto from Documented, ignore, name = "DtoSchema""#
)]
#[case::multipart(
    "ignored_metadata_multipart",
    r#"Dto from Documented, ignore, multipart, name = "DtoSchema""#
)]
fn ignored_metadata_matches_the_emitted_container(#[case] snapshot: &str, #[case] input: &str) {
    let storage = HashMap::from([(
        "Documented".to_string(),
        StructMetadata::new("Documented".to_string(), DOCUMENTED.to_string()),
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let (_, metadata) = generate_schema_type_code(&input, &storage).unwrap();
    let file: syn::File = syn::parse_str(&metadata.unwrap().definition).unwrap();
    insta::assert_snapshot!(snapshot, prettyplease::unparse(&file));
}

#[test]
fn raw_identifier_rename_and_add_targets() {
    let storage = HashMap::from([(
        "Source".to_string(),
        StructMetadata::new(
            "Source".to_string(),
            "pub struct Source { pub kind: i32 }".to_string(),
        ),
    )]);
    let input: SchemaTypeInput = syn::parse_str(
        r#"Dto from Source, rename = [("kind", "r#type")], add = [("r#match": u8)]"#,
    )
    .unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &storage).unwrap();
    let rendered = tokens.to_string();
    assert!(rendered.contains("pub r#type : i32"), "{rendered}");
    assert!(rendered.contains("pub r#match : u8"), "{rendered}");
}
