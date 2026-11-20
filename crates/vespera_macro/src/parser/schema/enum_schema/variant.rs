use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Schema, SchemaRef};

use super::super::{
    request_view::current_side,
    serde_attrs::{
        SerdeSide, extract_field_codec, extract_rename_all_for, extract_serde_skip_flags,
    },
    struct_schema::{
        NamedFields, collect_named_fields, positional_field_schema, tuple_fields_schema,
    },
    wire_override::CUSTOM_CODEC_DESCRIPTION,
};

/// Build the fields of a struct variant through the SAME field walker structs
/// use, so `skip`, `flatten`, `rename`, `skip_serializing_if`, doc comments and
/// `#[schema(...)]` constraints behave identically on enum variants.
pub(super) fn build_struct_variant_fields(
    fields_named: &syn::FieldsNamed,
    enum_rename_all_fields: Option<&str>,
    variant_attrs: &[syn::Attribute],
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> NamedFields {
    let variant_rename_all =
        extract_rename_all_for(variant_attrs, super::super::request_view::current_side());
    collect_named_fields(
        fields_named,
        variant_rename_all.as_deref().or(enum_rename_all_fields),
        false,
        false,
        known_schemas,
        struct_definitions,
    )
}

/// `true` when a variant never appears on the wire in the direction of the
/// view being built, so it is left out of the schema entirely:
/// `#[serde(skip)]`, plus `skip_serializing` in the response view or
/// `skip_deserializing` in the request (`{Name}Input`) view.
pub(super) fn is_variant_omitted(variant: &syn::Variant) -> bool {
    let flags = extract_serde_skip_flags(&variant.attrs);
    flags.skip
        || if super::super::request_view::in_request_view() {
            flags.skip_deserializing
        } else {
            flags.skip_serializing
        }
}

/// `true` when a variant is left out of BOTH the request and the response
/// views: `#[serde(skip)]`, or `skip_serializing` together with
/// `skip_deserializing`.
pub(super) fn is_variant_omitted_both_ways(variant: &syn::Variant) -> bool {
    let flags = extract_serde_skip_flags(&variant.attrs);
    flags.skip || (flags.skip_serializing && flags.skip_deserializing)
}

/// `true` for the `#[serde(other)]` catch-all unit variant.
pub(super) fn is_other_variant(variant: &syn::Variant) -> bool {
    extract_serde_skip_flags(&variant.attrs).other
}

/// `{}` for a variant whose content is written / read by a variant-level
/// `#[serde(with | serialize_with | deserialize_with = "..")]` on the side of
/// the view being built (its wire shape is unknown, see `wire_override`).
/// serde_derive checks the codec before the variant style, so a UNIT variant
/// with one goes over the wire as a newtype variant too (`{"Now": <custom>}`).
pub(super) fn variant_codec_schema(variant: &syn::Variant) -> Option<SchemaRef> {
    extract_field_codec(&variant.attrs)
        .custom_for(current_side())
        .then(|| {
            SchemaRef::Inline(Box::new(Schema {
                description: Some(CUSTOM_CODEC_DESCRIPTION.to_string()),
                ..Schema::default()
            }))
        })
}

/// `true` for a single-field tuple variant whose field is skipped on `side`
/// (`skip`, or `skip_serializing` for the response / `skip_deserializing` for
/// the request). serde_derive's `effective_style` writes and reads such a
/// newtype as a unit variant, in every representation.
pub(in crate::parser::schema) fn is_skipped_newtype(
    variant: &syn::Variant,
    side: SerdeSide,
) -> bool {
    match &variant.fields {
        syn::Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            let flags = extract_serde_skip_flags(&fields.unnamed[0].attrs);
            flags.skip
                || match side {
                    SerdeSide::Serialize => flags.skip_serializing,
                    SerdeSide::Deserialize => flags.skip_deserializing,
                }
        }
        _ => false,
    }
}

/// `true` when serde writes and reads the variant as a unit variant on the
/// current side: a unit variant, or a skipped newtype (see
/// [`is_skipped_newtype`]). A variant-level codec is checked before this.
pub(super) fn has_unit_style(variant: &syn::Variant) -> bool {
    matches!(variant.fields, syn::Fields::Unit) || is_skipped_newtype(variant, current_side())
}

/// `true` for a variant that is a unit on the wire on the current side
/// (no variant-level codec there, see [`variant_codec_schema`]).
pub(super) fn is_wire_unit(variant: &syn::Variant) -> bool {
    has_unit_style(variant) && !extract_field_codec(&variant.attrs).custom_for(current_side())
}

/// Schema of the single field of a newtype variant on the current side, its
/// `#[garde(..)]` rules and `#[schema(..)]` constraints applied.
pub(super) fn newtype_field_schema(
    field: &syn::Field,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    positional_field_schema(field, current_side(), known_schemas, struct_definitions)
}

/// Build a schema for a variant's data (tuple or struct fields)
pub(super) fn build_variant_data_schema(
    variant: &syn::Variant,
    renames: super::EnumRenames<'_>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<SchemaRef> {
    let codec = variant_codec_schema(variant);
    if codec.is_some() {
        return codec;
    }
    match &variant.fields {
        syn::Fields::Unit => None,
        // A skipped newtype is a unit variant on this side (no payload).
        syn::Fields::Unnamed(_) if is_skipped_newtype(variant, current_side()) => None,
        // Single field tuple variant - just the inner type
        syn::Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
            Some(newtype_field_schema(
                &fields_unnamed.unnamed[0],
                known_schemas,
                struct_definitions,
            ))
        }
        // Multiple fields tuple variant - array with prefixItems over the
        // positions emitted on the current side
        syn::Fields::Unnamed(fields_unnamed) => Some(SchemaRef::Inline(Box::new(
            tuple_fields_schema(fields_unnamed, false, known_schemas, struct_definitions),
        ))),
        syn::Fields::Named(fields_named) => Some(SchemaRef::Inline(Box::new(
            build_struct_variant_fields(
                fields_named,
                renames.fields,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(None, renames.deny_unknown_fields),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_named_field_uses_unknown_name() {
        let mut variant: syn::Variant = syn::parse_quote!(Broken { value: String });
        let syn::Fields::Named(fields) = &mut variant.fields else {
            panic!("named fields");
        };
        fields.named.first_mut().expect("field").ident = None;

        let fields = build_struct_variant_fields(
            fields,
            None,
            &[],
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );

        assert!(fields.properties.contains_key("unknown"));
        assert_eq!(fields.required, ["unknown"]);
    }

    #[test]
    fn empty_struct_variant_omits_properties_and_required() {
        let variant: syn::Variant = syn::parse_quote!(Empty {});
        let schema = build_variant_data_schema(
            &variant,
            super::super::EnumRenames {
                variants: None,
                fields: None,
                deny_unknown_fields: false,
            },
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        )
        .expect("struct variant schema");
        let SchemaRef::Inline(schema) = schema else {
            panic!("inline schema");
        };
        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }

    #[rstest::rstest]
    #[case::plain(syn::parse_quote!(A), false, false)]
    #[case::skip(syn::parse_quote!(#[serde(skip)] A), true, false)]
    #[case::skip_serializing(syn::parse_quote!(#[serde(skip_serializing)] A), true, false)]
    #[case::skip_deserializing(syn::parse_quote!(#[serde(skip_deserializing)] A), false, false)]
    #[case::other(syn::parse_quote!(#[serde(other)] Unknown), false, true)]
    fn variant_level_flags(
        #[case] variant: syn::Variant,
        #[case] omitted: bool,
        #[case] other: bool,
    ) {
        assert_eq!(is_variant_omitted(&variant), omitted);
        assert_eq!(is_other_variant(&variant), other);
    }

    /// R32-P2-1: only a single-field tuple variant whose field is skipped on
    /// the given side is a skipped newtype there (serde's `effective_style`).
    #[rstest::rstest]
    #[case::skip_serialize(syn::parse_quote!(V(#[serde(skip)] u8)), SerdeSide::Serialize, true)]
    #[case::skip_deserialize(syn::parse_quote!(V(#[serde(skip)] u8)), SerdeSide::Deserialize, true)]
    #[case::skip_serializing_on_serialize(
        syn::parse_quote!(V(#[serde(skip_serializing)] u8)), SerdeSide::Serialize, true
    )]
    #[case::skip_serializing_on_deserialize(
        syn::parse_quote!(V(#[serde(skip_serializing)] u8)), SerdeSide::Deserialize, false
    )]
    #[case::skip_deserializing_on_serialize(
        syn::parse_quote!(V(#[serde(skip_deserializing)] u8)), SerdeSide::Serialize, false
    )]
    #[case::skip_deserializing_on_deserialize(
        syn::parse_quote!(V(#[serde(skip_deserializing)] u8)), SerdeSide::Deserialize, true
    )]
    #[case::unskipped_newtype(syn::parse_quote!(V(u8)), SerdeSide::Serialize, false)]
    #[case::unit(syn::parse_quote!(V), SerdeSide::Deserialize, false)]
    #[case::multi_field(syn::parse_quote!(V(#[serde(skip)] u8, u8)), SerdeSide::Serialize, false)]
    #[case::struct_variant(syn::parse_quote!(V { #[serde(skip)] x: u8 }), SerdeSide::Serialize, false)]
    fn skipped_newtype_per_side(
        #[case] variant: syn::Variant,
        #[case] side: SerdeSide,
        #[case] expected: bool,
    ) {
        assert_eq!(is_skipped_newtype(&variant, side), expected);
    }
}
