//! Schemas for members whose serde wire shape is NOT their Rust type's.
//!
//! - `#[schema(value_type = T)]` (field or container) documents `T` instead.
//! - Container `#[serde(into = "T")]` serializes as `T` (the response view),
//!   `#[serde(from = "T")]` / `try_from` deserializes from `T` (the request
//!   view, see [`super::request_view`]); an explicit container
//!   `#[schema(value_type = ..)]` wins over both.
//! - A field written / read by a custom serde function (`with`,
//!   `serialize_with`, `deserialize_with`) without a `value_type` has an
//!   unknowable shape. It is documented as any value (`{}`) with a
//!   description saying why — never as the declared type, which would be a
//!   confident lie (`ts_seconds` turns a date-time string into an integer).

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Schema, SchemaRef};

use super::{
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{SerdeSide, extract_container_conversion, extract_field_codec},
    type_schema::parse_type_to_schema_ref,
};

/// Description of a field whose custom serde function hides its wire shape.
pub const CUSTOM_CODEC_DESCRIPTION: &str = "Converted by a custom serde function \
(`with` / `serialize_with` / `deserialize_with`); its wire shape is not known. \
Add `#[schema(value_type = T)]` to document it.";

/// Schema of `field` on `side`, honouring `#[schema(any)]`,
/// `#[schema(value_type = T)]` and the serde `with` family.
pub fn field_schema_ref(
    field: &syn::Field,
    constraints: &SchemaConstraints,
    side: SerdeSide,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    // `#[schema(any)]` is the "any JSON value" (`{}`) escape hatch.
    if constraints.is_any() {
        return SchemaRef::Inline(Box::default());
    }
    if let Some(value_type) = &constraints.value_type {
        return parse_type_to_schema_ref(&value_type.0, known_schemas, struct_definitions);
    }
    if extract_field_codec(&field.attrs).custom_for(side) {
        return SchemaRef::Inline(Box::new(Schema {
            description: Some(CUSTOM_CODEC_DESCRIPTION.to_string()),
            ..Schema::default()
        }));
    }
    parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions)
}

/// The type a container is documented as on `side` instead of its own
/// fields / variants: `#[schema(value_type = T)]`, else serde's `into`
/// (serialize) or `from` / `try_from` (deserialize).
pub fn container_override_type(attrs: &[syn::Attribute], side: SerdeSide) -> Option<syn::Type> {
    extract_schema_constraints(attrs)
        .value_type
        .map(|value_type| value_type.0)
        .or_else(|| extract_container_conversion(attrs, side))
}

/// [`container_override_type`] for the view currently being built.
pub fn current_container_override(attrs: &[syn::Attribute]) -> Option<syn::Type> {
    container_override_type(attrs, super::request_view::current_side())
}

#[cfg(test)]
mod tests {
    use quote::ToTokens;
    use rstest::rstest;

    use super::*;

    fn field(src: &str) -> syn::Field {
        let item: syn::ItemStruct = syn::parse_str(&format!("struct S {{ {src} }}")).unwrap();
        item.fields.into_iter().next().unwrap()
    }

    #[rstest]
    #[case::plain("plain", "x: DateTime<Utc>", SerdeSide::Serialize)]
    #[case::any(
        "any",
        "#[schema(any)] #[serde(with = \"m\")] x: u8",
        SerdeSide::Serialize
    )]
    #[case::value_type(
        "value_type",
        "#[schema(value_type = i64)] #[serde(with = \"ts_seconds\")] x: DateTime<Utc>",
        SerdeSide::Serialize
    )]
    #[case::value_type_option(
        "value_type_option",
        "#[schema(value_type = Option<i64>)] #[serde(with = \"ts\")] x: Option<DateTime<Utc>>",
        SerdeSide::Deserialize
    )]
    #[case::with_unknown(
        "with_unknown",
        "#[serde(with = \"m\")] x: DateTime<Utc>",
        SerdeSide::Serialize
    )]
    #[case::serialize_with_on_deserialize(
        "serialize_with_on_deserialize",
        "#[serde(serialize_with = \"f\")] x: u8",
        SerdeSide::Deserialize
    )]
    #[case::deserialize_with_on_deserialize(
        "deserialize_with_on_deserialize",
        "#[serde(deserialize_with = \"f\")] x: u8",
        SerdeSide::Deserialize
    )]
    fn field_schemas(#[case] name: &str, #[case] src: &str, #[case] side: SerdeSide) {
        let field = field(src);
        let constraints = extract_schema_constraints(&field.attrs);
        let schema = field_schema_ref(
            &field,
            &constraints,
            side,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        insta::assert_snapshot!(
            format!("wire_override_field_{name}"),
            format!(
                "{src} ({side:?}) => {}",
                serde_json::to_string(&schema).unwrap()
            )
        );
    }

    #[rstest]
    #[case::none("#[serde(rename_all = \"camelCase\")]", None, None)]
    #[case::into("#[serde(into = \"String\")]", Some("String"), None)]
    #[case::from("#[serde(from = \"Raw\")]", None, Some("Raw"))]
    #[case::try_from_and_into(
        "#[serde(into = \"Out\", try_from = \"In\")]",
        Some("Out"),
        Some("In")
    )]
    #[case::value_type_wins(
        "#[schema(value_type = Wire)] #[serde(into = \"Out\", from = \"In\")]",
        Some("Wire"),
        Some("Wire")
    )]
    fn container_overrides(
        #[case] src: &str,
        #[case] serialize: Option<&str>,
        #[case] deserialize: Option<&str>,
    ) {
        let attrs = syn::parse_str::<syn::ItemStruct>(&format!("{src} struct S;"))
            .unwrap()
            .attrs;
        let render =
            |side| container_override_type(&attrs, side).map(|ty| ty.to_token_stream().to_string());
        assert_eq!(render(SerdeSide::Serialize).as_deref(), serialize);
        assert_eq!(render(SerdeSide::Deserialize).as_deref(), deserialize);
        assert_eq!(
            current_container_override(&attrs).map(|ty| ty.to_token_stream().to_string()),
            serialize.map(str::to_string)
        );
        let in_request = super::super::request_view::with_request_view(|| {
            current_container_override(&attrs).map(|ty| ty.to_token_stream().to_string())
        });
        assert_eq!(in_request.as_deref(), deserialize);
    }
}
