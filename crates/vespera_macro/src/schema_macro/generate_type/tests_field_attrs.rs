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
