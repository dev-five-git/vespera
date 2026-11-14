//! Wire-name fidelity of struct schemas.
//!
//! `#[serde(tag = "...")]` on a struct with named fields: serde WRITES the
//! tag (the container's serialize name) but never reads it, so only the
//! response view documents it. Multipart form fields are named the way the
//! `Multipart` derive reads them.

use rstest::rstest;

use super::*;
use crate::parser::schema::request_view::with_request_view;

fn render(src: &str, request: bool) -> String {
    let known = HashSet::from(["Meta".to_string()]);
    let defs = HashMap::from([("Meta".to_string(), "struct Meta { at: String }".to_string())]);
    let item: syn::ItemStruct = syn::parse_str(src).unwrap();
    let build = || parse_struct_to_schema(&item, &known, &defs);
    let schema = if request {
        with_request_view(build)
    } else {
        build()
    };
    serde_json::to_string_pretty(&schema).unwrap()
}

#[rstest]
#[case::plain("plain", r#"#[serde(tag = "type")] struct Created { id: u32 }"#)]
#[case::container_rename(
    "container_rename",
    r#"#[serde(tag = "kind", rename = "created")] struct Created { id: u32 }"#
)]
#[case::sided_rename(
    "sided_rename",
    r#"#[serde(tag = "kind", rename(serialize = "out", deserialize = "in"))] struct Created { id: u32 }"#
)]
#[case::deny_unknown_fields(
    "deny_unknown_fields",
    r#"#[serde(tag = "type", deny_unknown_fields)] struct Created { id: u32 }"#
)]
#[case::flatten(
    "flatten",
    r#"#[serde(tag = "type")] struct Created { id: u32, #[serde(flatten)] meta: Meta }"#
)]
#[case::no_fields("no_fields", r#"#[serde(tag = "type")] struct Ping {}"#)]
#[case::raw_ident("raw_ident", r#"#[serde(tag = "t")] struct r#Match { id: u32 }"#)]
// The shape `schema_type!` emits for a tagged source: the DTO is renamed to
// the source's serde name, so the documented tag value stays the source's.
#[case::schema_type_dto(
    "schema_type_dto",
    r#"#[serde(rename_all = "camelCase")] #[serde(tag = "type")] #[serde(rename = "Created")] pub struct CreatedDto { user_id: u32 }"#
)]
fn struct_tag(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(
        format!("struct_tag_{name}"),
        format!(
            "response:\n{}\nrequest:\n{}",
            render(src, false),
            render(src, true)
        )
    );
}

/// Multipart form field names: the request view uses the `Multipart`
/// derive's order (`form_data(field_name)` > deserialize-side serde rename >
/// deserialize-side `rename_all`); the response view keeps serde's names.
#[rstest]
#[case::form_data_over_serde_rename(
    "form_data_over_serde_rename",
    r#"struct Upload { #[serde(rename = "a")] #[form_data(field_name = "b")] file: String }"#
)]
#[case::sided_rename(
    "sided_rename",
    r#"struct Upload { #[serde(rename(serialize = "out", deserialize = "in"))] file: String }"#
)]
#[case::sided_rename_all(
    "sided_rename_all",
    r#"#[serde(rename_all(serialize = "SCREAMING_SNAKE_CASE", deserialize = "camelCase"))] struct Upload { file_name: String, #[form_data(field_name = "doc")] doc_file: String }"#
)]
fn multipart_field_names(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(
        format!("multipart_field_names_{name}"),
        format!(
            "response:\n{}\nrequest:\n{}",
            render(src, false),
            render(src, true)
        )
    );
}
