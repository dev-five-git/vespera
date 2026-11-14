//! Field references that conflict with the rest of the invocation (M13 /
//! L4): each is an error spanned at the offending name's literal.

use std::collections::HashMap;

use rstest::rstest;

use super::*;
use crate::schema_macro::SchemaTypeInput;

const SOURCE: &str = "pub struct Source { pub id: i32, pub name: String, pub owner: OwnerEntity }";

fn error(input: &str) -> syn::Error {
    let storage = HashMap::from([(
        "Source".to_string(),
        StructMetadata::new("Source".to_string(), SOURCE.to_string()),
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    generate_schema_type_code(&input, &storage).unwrap_err()
}

#[rstest]
#[case::rename_omitted(
    r#"Dto from Source, omit = ["name"], rename = [("name", "title")]"#,
    "`rename` names field `name`, which this type does not include: `omit` drops it"
)]
#[case::partial_unpicked(
    r#"Dto from Source, pick = ["id"], partial = ["name"]"#,
    "`partial` names field `name`, which this type does not include: `pick` does not list it"
)]
#[case::adapter_unknown(
    r#"Dto from Source, relation_adapters = [("nope", Card)]"#,
    "relation adapter field `nope` does not exist in type `Source`. Available fields: [\"id\", \"name\", \"owner\"]"
)]
#[case::adapter_twice(
    r#"Dto from Source, relation_adapters = [("id", Card), ("id", Card)]"#,
    "`relation_adapters` lists field `id` more than once"
)]
#[case::adapter_plain(
    r#"Dto from Source, relation_adapters = [("name", Card)]"#,
    "`relation_adapters`: field `name` of `Source` is not a HasOne / BelongsTo SeaORM relation this type includes"
)]
#[case::adapter_multipart(
    r#"Dto from Source, multipart, relation_adapters = [("name", Card)]"#,
    "`relation_adapters`: field `name` of `Source` is not a HasOne / BelongsTo SeaORM relation this type includes (`multipart` types carry no relations)"
)]
fn conflicting_field_references(#[case] input: &str, #[case] expected: &str) {
    // The spans are locked by the trybuild case
    // `tests/ui/schema_type_conflicting_refs.rs`.
    assert_eq!(error(input).to_string(), expected);
}

/// Without a recorded span (an input not parsed from tokens) the error
/// falls back to the source type.
#[test]
fn unrecorded_span_falls_back_to_source_type() {
    let ty: syn::Type = syn::parse_str("Source").unwrap();
    let spans = crate::schema_macro::input::ParamSpans::default();
    let at = SpannedSource {
        source_type: &ty,
        spans: &spans,
    };
    let err = at.error_at("pick", "x", "boom".to_string());
    assert_eq!(err.to_string(), "boom");
}
