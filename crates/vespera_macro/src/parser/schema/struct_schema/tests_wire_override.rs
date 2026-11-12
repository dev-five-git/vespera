//! Struct / enum schemas whose serde wire shape is not the Rust type's:
//! `#[schema(value_type)]`, serde `with` family, container `into` / `from` /
//! `try_from` — in both the response and the request (`{Name}Input`) view.

use rstest::rstest;

use super::*;
use crate::parser::schema::{parse_enum_to_schema, request_view::with_request_view};

fn render(src: &str, request: bool) -> String {
    let known = HashSet::from(["Wire".to_string(), "Raw".to_string()]);
    let defs = HashMap::<String, String>::new();
    let build = || {
        syn::parse_str::<syn::ItemEnum>(src).map_or_else(
            |_| parse_struct_to_schema(&syn::parse_str(src).unwrap(), &known, &defs),
            |item| parse_enum_to_schema(&item, &known, &defs),
        )
    };
    let schema = if request {
        with_request_view(build)
    } else {
        build()
    };
    serde_json::to_string_pretty(&schema).unwrap()
}

#[rstest]
#[case::field_value_type(
    "field_value_type",
    r#"struct Event {
        /// Unix seconds.
        #[serde(with = "chrono::serde::ts_seconds")]
        #[schema(value_type = i64, minimum = 0)]
        at: DateTime<Utc>,
        #[serde(with = "chrono::serde::ts_seconds_option")]
        #[schema(value_type = Option<i64>)]
        until: Option<DateTime<Utc>>,
        #[schema(value_type = Vec<Wire>)]
        wires: Vec<String>,
    }"#
)]
#[case::field_with_unknown(
    "field_with_unknown",
    r#"struct Event {
        #[serde(with = "hex")]
        raw: Vec<u8>,
        /// Documented by hand.
        #[serde(serialize_with = "f")]
        out_only: u8,
        #[serde(deserialize_with = "g")]
        in_only: u8,
        #[serde(deserialize_with = "g")]
        maybe: Option<u8>,
    }"#
)]
#[case::container_into(
    "container_into",
    r#"/// A colour.
    #[serde(into = "String")]
    struct Rgb { r: u8, g: u8, b: u8 }"#
)]
#[case::container_into_and_try_from(
    "container_into_and_try_from",
    r#"#[serde(into = "Wire", try_from = "Raw")] struct Money { cents: i64 }"#
)]
#[case::container_value_type(
    "container_value_type",
    r#"#[schema(value_type = Wire)] #[serde(into = "String", from = "Raw")] struct Money { cents: i64 }"#
)]
#[case::newtype_with(
    "newtype_with",
    r#"struct Stamp(#[serde(with = "ts_seconds")] #[schema(value_type = i64)] DateTime<Utc>);"#
)]
#[case::transparent_with(
    "transparent_with",
    r#"#[serde(transparent)] struct Stamp { #[serde(with = "ts_seconds")] at: DateTime<Utc> }"#
)]
#[case::enum_into(
    "enum_into",
    r#"#[serde(into = "u8", from = "Raw")] enum Level { Low, High }"#
)]
fn wire_override_views(#[case] name: &str, #[case] src: &str) {
    insta::assert_snapshot!(
        format!("wire_override_{name}"),
        format!(
            "{src}\n\n--- response ---\n{}\n--- request ---\n{}",
            render(src, false),
            render(src, true)
        )
    );
}
