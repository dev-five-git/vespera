//! Schemas for members whose serde wire shape is NOT their Rust type's.
//!
//! - `#[schema(value_type = T)]` (field or container) documents `T` instead.
//! - Container `#[serde(into = "T")]` serializes as `T` (the response view),
//!   `#[serde(from = "T")]` / `try_from` deserializes from `T` (the request
//!   view, see [`super::request_view`]); an explicit container
//!   `#[schema(value_type = ..)]` wins over both.
//! - A field whose serde `with` / `serialize_with` / `deserialize_with` runs a
//!   custom codec is documented on each side the codec applies to (see
//!   [`codec_wire`]), in this order:
//!   1. a well-known codec module from [`CODEC_TABLE`] (chrono, time,
//!      humantime, serde_with, serde_bytes) documents the shape it writes;
//!   2. a string type (`String`, `&str`, `Cow<str>`, `Box<str>`, `Arc<str>`,
//!      or an `Option` of one) is documented as a string, which assumes the
//!      codec keeps the string a string (trimming, lower-casing, empty as
//!      `None`);
//!   3. anything else is a compile error (see [`unknown_codec_error`]): its
//!      wire shape is unknown, so the field needs `#[schema(value_type = ..)]`.
//!
//! `value_type` always wins and `#[schema(any)]` documents any JSON value.
//! A codec on one side only changes only that side's view.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use quote::ToTokens;
use vespera_core::schema::{Schema, SchemaRef};

use super::{
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{
        SerdeSide, extract_codec_module, extract_container_conversion, extract_field_codec,
    },
    type_schema::parse_type_to_schema_ref,
};
use crate::schema_macro::type_utils::is_option_type;

/// Description of a field whose custom serde function hides its wire shape.
pub const CUSTOM_CODEC_DESCRIPTION: &str = "Converted by a custom serde function \
(`with` / `serialize_with` / `deserialize_with`); its wire shape is not known. \
Add `#[schema(value_type = T)]` to document it.";

/// Well-known codec modules (the path a `with` names, or a `serialize_with` /
/// `deserialize_with` path minus its final fn segment) and the Rust type
/// naming the shape they write / read. Matched on the full path only.
///
/// Each `_option` variant also accepts `null`. `serde_bytes` writes a byte
/// buffer as an array of integers `0..=255`, which is what `serde_json` emits.
pub const CODEC_TABLE: &[(&str, &str)] = &[
    ("chrono::serde::ts_seconds", "i64"),
    ("chrono::serde::ts_seconds_option", "Option<i64>"),
    ("chrono::serde::ts_milliseconds", "i64"),
    ("chrono::serde::ts_milliseconds_option", "Option<i64>"),
    ("chrono::serde::ts_microseconds", "i64"),
    ("chrono::serde::ts_microseconds_option", "Option<i64>"),
    ("chrono::serde::ts_nanoseconds", "i64"),
    ("chrono::serde::ts_nanoseconds_option", "Option<i64>"),
    ("time::serde::rfc3339", "DateTime<Utc>"),
    ("time::serde::rfc3339::option", "Option<DateTime<Utc>>"),
    ("time::serde::iso8601", "DateTime<Utc>"),
    ("time::serde::iso8601::option", "Option<DateTime<Utc>>"),
    ("time::serde::timestamp", "i64"),
    ("time::serde::timestamp::option", "Option<i64>"),
    ("humantime_serde", "String"),
    ("humantime_serde::option", "Option<String>"),
    ("serde_with::rust::display_fromstr", "String"),
    ("serde_bytes", "Vec<u8>"),
];

/// What a field's custom serde codec on one side documents.
pub enum CodecWire {
    /// No codec on this side: the field's Rust type is documented.
    RustType,
    /// The shape a well-known codec or a string type writes / reads.
    Documented(Box<syn::Type>),
    /// A codec whose wire shape is unknown and not overridden.
    Unknown,
}

/// Schema of `field` on `side`, honouring `#[schema(any)]`,
/// `#[schema(value_type = T)]` and the serde `with` family (see [`codec_wire`]).
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
    match codec_wire(field, side) {
        CodecWire::RustType => {
            parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions)
        }
        CodecWire::Documented(ty) => {
            parse_type_to_schema_ref(&ty, known_schemas, struct_definitions)
        }
        // Only reachable when `#[derive(Schema)]` did not reject the field
        // (see `unknown_codec_error`): documented as any value.
        CodecWire::Unknown => SchemaRef::Inline(Box::new(Schema {
            description: Some(CUSTOM_CODEC_DESCRIPTION.to_string()),
            ..Schema::default()
        })),
    }
}

/// What the custom serde codec of `field` documents on `side`.
pub fn codec_wire(field: &syn::Field, side: SerdeSide) -> CodecWire {
    if !extract_field_codec(&field.attrs).custom_for(side) {
        return CodecWire::RustType;
    }
    let module = extract_codec_module(&field.attrs, side).join("::");
    if let Some((_, wire)) = CODEC_TABLE.iter().find(|(path, _)| *path == module) {
        return CodecWire::Documented(Box::new(parse_wire_type(wire)));
    }
    if is_string_type(&field.ty) {
        let wire = if is_option_type(&field.ty) {
            "Option<String>"
        } else {
            "String"
        };
        return CodecWire::Documented(Box::new(parse_wire_type(wire)));
    }
    CodecWire::Unknown
}

/// Whether the field's custom serde function on `side` changes its shape: a
/// `with` / `serialize_with` / `deserialize_with` on that side. `value_type` /
/// `#[schema(any)]` are the caller's to check.
pub fn codec_applies(attrs: &[syn::Attribute], side: SerdeSide) -> bool {
    extract_field_codec(attrs).custom_for(side)
}

/// The documented type of `field` on `side` as tokens, `None` when it is
/// [`CodecWire::Unknown`]. Two views whose tokens differ document different
/// schemas for the field.
pub fn documented_tokens(field: &syn::Field, side: SerdeSide) -> Option<String> {
    match codec_wire(field, side) {
        CodecWire::RustType => Some(field.ty.to_token_stream().to_string()),
        CodecWire::Documented(ty) => Some(ty.to_token_stream().to_string()),
        CodecWire::Unknown => None,
    }
}

/// The compile error for `field` (named `name` in the message) whose codec on
/// either side has an unknown wire shape and no `value_type` / `any` override.
/// Spanned on the `#[serde(..)]` attribute holding the codec.
pub fn unknown_codec_error(field: &syn::Field, name: &str) -> Option<syn::Error> {
    let constraints = extract_schema_constraints(&field.attrs);
    if constraints.is_any() || constraints.value_type.is_some() {
        return None;
    }
    [SerdeSide::Serialize, SerdeSide::Deserialize]
        .into_iter()
        .find(|side| matches!(codec_wire(field, *side), CodecWire::Unknown))
        .and_then(|side| {
            field
                .attrs
                .iter()
                .find(|attr| codec_applies(std::slice::from_ref(attr), side))
        })
        .map(|attr| {
            syn::Error::new_spanned(
                attr,
                format!(
                    "`{name}` uses `#[serde(with = \"..\")]` (or deserialize_with/serialize_with), \
so its wire shape is unknown; add `#[schema(value_type = ..)]` (e.g. `value_type = String`, \
or `serde_json::Value` for any JSON)"
                ),
            )
        })
}

/// The first [`unknown_codec_error`] among a derived type's fields: struct
/// fields, and the fields of enum variants (named `Variant::field`). A
/// container `#[schema(value_type = T)]` documents the whole type, so its
/// fields are never documented and never an error.
pub fn derive_codec_error(input: &syn::DeriveInput) -> Option<syn::Error> {
    if extract_schema_constraints(&input.attrs)
        .value_type
        .is_some()
    {
        return None;
    }
    let first_error = |fields: &syn::Fields, prefix: &str| {
        fields.iter().enumerate().find_map(|(index, field)| {
            let name = field
                .ident
                .as_ref()
                .map_or_else(|| index.to_string(), ToString::to_string);
            unknown_codec_error(field, &format!("{prefix}{name}"))
        })
    };
    match &input.data {
        syn::Data::Struct(data) => first_error(&data.fields, ""),
        syn::Data::Enum(data) => data
            .variants
            .iter()
            .find_map(|variant| first_error(&variant.fields, &format!("{}::", variant.ident))),
        syn::Data::Union(_) => None,
    }
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

/// A type text from [`CODEC_TABLE`] or a constant string type.
fn parse_wire_type(text: &str) -> syn::Type {
    syn::parse_str(text).expect("codec wire types are valid Rust types")
}

/// A string type: `String`, `str` (behind `&`), or `Cow` / `Box` / `Arc` /
/// `Option` of one (any path ending in those idents).
fn is_string_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => is_string_type(&reference.elem),
        syn::Type::Path(path) => path.path.segments.last().is_some_and(string_segment),
        _ => false,
    }
}

fn string_segment(segment: &syn::PathSegment) -> bool {
    match segment.ident.to_string().as_str() {
        "String" | "str" => true,
        "Option" | "Cow" | "Box" | "Arc" => {
            last_type_argument(&segment.arguments).is_some_and(is_string_type)
        }
        _ => false,
    }
}

/// The last type argument of `<..>` (skipping lifetimes).
fn last_type_argument(arguments: &syn::PathArguments) -> Option<&syn::Type> {
    let syn::PathArguments::AngleBracketed(args) = arguments else {
        return None;
    };
    args.args.iter().rev().find_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
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

    #[rstest]
    #[case::struct_field(
        "struct S { ok: u8, #[serde(with = \"hex\")] checksum: Vec<u8> }",
        Some("`checksum` uses")
    )]
    #[case::tuple_field("struct S(#[serde(with = \"hex\")] Vec<u8>);", Some("`0` uses"))]
    #[case::enum_variant_field(
        "enum E { Ok(u8), Pair(String, #[serde(deserialize_with = \"f\")] u8) }",
        Some("`Pair::1` uses")
    )]
    #[case::enum_clean("enum E { A { x: u8 }, B(String) }", None)]
    #[case::union_ignored("union U { a: u8 }", None)]
    #[case::container_value_type(
        "#[schema(value_type = i64)] struct S(#[serde(with = \"hex\")] Vec<u8>);",
        None
    )]
    fn derive_codec_errors(#[case] src: &str, #[case] message: Option<&str>) {
        let input: syn::DeriveInput = syn::parse_str(src).unwrap();
        let error = derive_codec_error(&input);
        assert_eq!(error.is_some(), message.is_some(), "{src}");
        if let (Some(error), Some(message)) = (error, message) {
            assert!(error.to_string().starts_with(message), "{error}");
        }
    }

    fn wire_tokens(src: &str, side: SerdeSide) -> Option<String> {
        documented_tokens(&field(src), side)
    }

    #[rstest]
    #[case::ts_seconds("chrono::serde::ts_seconds", "i64")]
    #[case::ts_seconds_option("chrono::serde::ts_seconds_option", "Option < i64 >")]
    #[case::ts_milliseconds("chrono::serde::ts_milliseconds", "i64")]
    #[case::ts_milliseconds_option("chrono::serde::ts_milliseconds_option", "Option < i64 >")]
    #[case::ts_microseconds("chrono::serde::ts_microseconds", "i64")]
    #[case::ts_microseconds_option("chrono::serde::ts_microseconds_option", "Option < i64 >")]
    #[case::ts_nanoseconds("chrono::serde::ts_nanoseconds", "i64")]
    #[case::ts_nanoseconds_option("chrono::serde::ts_nanoseconds_option", "Option < i64 >")]
    #[case::rfc3339("time::serde::rfc3339", "DateTime < Utc >")]
    #[case::rfc3339_option("time::serde::rfc3339::option", "Option < DateTime < Utc > >")]
    #[case::iso8601("time::serde::iso8601", "DateTime < Utc >")]
    #[case::iso8601_option("time::serde::iso8601::option", "Option < DateTime < Utc > >")]
    #[case::timestamp("time::serde::timestamp", "i64")]
    #[case::timestamp_option("time::serde::timestamp::option", "Option < i64 >")]
    #[case::humantime("humantime_serde", "String")]
    #[case::humantime_option("humantime_serde::option", "Option < String >")]
    #[case::display_fromstr("serde_with::rust::display_fromstr", "String")]
    #[case::serde_bytes("serde_bytes", "Vec < u8 >")]
    #[case::leading_colons("::chrono::serde::ts_seconds", "i64")]
    fn codec_table_entries(#[case] module: &str, #[case] expected: &str) {
        let src = format!("#[serde(with = \"{module}\")] x: u8");
        assert_eq!(
            wire_tokens(&src, SerdeSide::Serialize).as_deref(),
            Some(expected)
        );
        assert_eq!(
            wire_tokens(&src, SerdeSide::Deserialize).as_deref(),
            Some(expected)
        );
    }

    #[rstest]
    #[case::serialize_with_module_fn(
        "#[serde(serialize_with = \"chrono::serde::ts_seconds::serialize\")] x: DateTime<Utc>",
        SerdeSide::Serialize,
        Some("i64")
    )]
    #[case::serialize_with_side_only(
        "#[serde(serialize_with = \"chrono::serde::ts_seconds::serialize\")] x: DateTime<Utc>",
        SerdeSide::Deserialize,
        Some("DateTime < Utc >")
    )]
    #[case::deserialize_with_module_fn(
        "#[serde(deserialize_with = \"time::serde::rfc3339::deserialize\")] x: u8",
        SerdeSide::Deserialize,
        Some("DateTime < Utc >")
    )]
    #[case::no_codec("x: DateTime<Utc>", SerdeSide::Serialize, Some("DateTime < Utc >"))]
    fn codec_sides(#[case] src: &str, #[case] side: SerdeSide, #[case] expected: Option<&str>) {
        assert_eq!(wire_tokens(src, side).as_deref(), expected);
    }

    #[rstest]
    #[case::string("String", "String")]
    #[case::str_ref("&str", "String")]
    #[case::cow("Cow<str>", "String")]
    #[case::cow_lifetime("Cow<'a, str>", "String")]
    #[case::box_str("Box<str>", "String")]
    #[case::arc_str("Arc<str>", "String")]
    #[case::path_string("std::string::String", "String")]
    #[case::option_string("Option<String>", "Option < String >")]
    #[case::option_cow("Option<Cow<str>>", "Option < String >")]
    fn string_types_document_a_string(#[case] ty: &str, #[case] expected: &str) {
        let src = format!("#[serde(with = \"hex_bytes\")] x: {ty}");
        assert_eq!(
            wire_tokens(&src, SerdeSide::Serialize).as_deref(),
            Some(expected)
        );
    }

    #[rstest]
    #[case::unknown_module("#[serde(with = \"hex_bytes\")] x: Vec<u8>")]
    #[case::bare_fn("#[serde(serialize_with = \"f\")] x: Vec<u8>")]
    #[case::unimported_table_name("#[serde(with = \"ts_seconds\")] x: DateTime<Utc>")]
    #[case::non_string_option("#[serde(with = \"m\")] x: Option<Vec<u8>>")]
    #[case::box_vec("#[serde(with = \"m\")] x: Box<Vec<u8>>")]
    #[case::bare_cow("#[serde(with = \"m\")] x: Cow")]
    #[case::bare_option("#[serde(with = \"m\")] x: Option")]
    #[case::lifetime_only_cow("#[serde(with = \"m\")] x: Cow<'a>")]
    #[case::reference_vec("#[serde(with = \"m\")] x: &Vec<u8>")]
    #[case::tuple("#[serde(with = \"m\")] x: (u8, u8)")]
    fn unknown_shapes(#[case] src: &str) {
        assert!(matches!(
            codec_wire(&field(src), SerdeSide::Serialize),
            CodecWire::Unknown
        ));
        assert_eq!(wire_tokens(src, SerdeSide::Serialize), None);
    }

    #[rstest]
    #[case::unknown("#[serde(with = \"hex_bytes\")] checksum: Vec<u8>", true)]
    #[case::deserialize_side("#[serde(deserialize_with = \"f\")] checksum: Vec<u8>", true)]
    #[case::serialize_side("#[serde(serialize_with = \"f\")] checksum: u8", true)]
    #[case::string_ok("#[serde(with = \"m\")] checksum: String", false)]
    #[case::table_ok("#[serde(with = \"serde_bytes\")] checksum: Vec<u8>", false)]
    #[case::value_type(
        "#[schema(value_type = String)] #[serde(with = \"hex_bytes\")] checksum: Vec<u8>",
        false
    )]
    #[case::any(
        "#[schema(any)] #[serde(with = \"hex_bytes\")] checksum: Vec<u8>",
        false
    )]
    #[case::no_codec("checksum: Vec<u8>", false)]
    fn unknown_codec_errors(#[case] src: &str, #[case] expected: bool) {
        let error = unknown_codec_error(&field(src), "checksum");
        assert_eq!(error.is_some(), expected, "{src}");
        if let Some(error) = error {
            assert!(
                error
                    .to_string()
                    .starts_with("`checksum` uses `#[serde(with = \"..\")]`"),
                "{error}"
            );
        }
    }

    #[test]
    fn codec_applies_per_side() {
        let f = field("#[serde(serialize_with = \"f\")] x: u8");
        assert!(codec_applies(&f.attrs, SerdeSide::Serialize));
        assert!(!codec_applies(&f.attrs, SerdeSide::Deserialize));
    }

    #[rstest]
    #[case::table(
        "#[serde(with = \"chrono::serde::ts_seconds\")] x: DateTime<Utc>",
        "wire_override_field_codec_table"
    )]
    #[case::string(
        "#[serde(with = \"m\")] x: Option<String>",
        "wire_override_field_codec_string"
    )]
    fn documented_field_schemas(#[case] src: &str, #[case] name: &str) {
        let field = field(src);
        let constraints = extract_schema_constraints(&field.attrs);
        let schema = field_schema_ref(
            &field,
            &constraints,
            SerdeSide::Serialize,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        insta::assert_snapshot!(
            name.to_string(),
            format!("{src} => {}", serde_json::to_string(&schema).unwrap())
        );
    }
}
