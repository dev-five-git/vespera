use super::*;
use crate::metadata::StructMetadata;
use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

fn to_storage(items: Vec<StructMetadata>) -> std::collections::HashMap<String, StructMetadata> {
    items.into_iter().map(|s| (s.name.clone(), s)).collect()
}

/// R30-SMB1-L1: a `cfg_attr`-gated serde default leaves the generated default
/// fn unreferenced in the build where the override applies, so that fn is
/// allowed to be dead; an ungated default fn is not.
#[rstest::rstest]
#[case::gated(
    "#[sea_orm(default_value = 5)] #[cfg_attr(feature = \"x\", serde(default))] pub n: i32",
    true
)]
#[case::ungated("#[sea_orm(default_value = 5)] pub n: i32", false)]
fn cfg_gated_serde_default_fn_may_be_unused(#[case] field: &str, #[case] allowed: bool) {
    let definition = format!("pub struct Model {{ {field} }}");
    let storage = to_storage(vec![StructMetadata::new(
        "Model".to_string(),
        definition.clone(),
    )]);
    let input: SchemaTypeInput = syn::parse2(quote!(Dto from Model)).unwrap();
    let output = generate_schema_type_code(&input, &storage)
        .unwrap()
        .0
        .to_string();
    assert!(output.contains("fn default_Dto_n"), "{output}");
    assert_eq!(
        output.contains("# [allow (dead_code)]"),
        allowed,
        "{output}"
    );
}
