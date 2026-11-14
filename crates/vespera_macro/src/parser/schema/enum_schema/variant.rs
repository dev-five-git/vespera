use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Schema, SchemaRef};

use super::super::{
    request_view::current_side,
    schema_attrs::extract_schema_constraints,
    serde_attrs::{extract_field_codec, extract_rename_all_for, extract_serde_skip_flags},
    struct_schema::{NamedFields, collect_named_fields, tuple_fields_schema},
    wire_override::{CUSTOM_CODEC_DESCRIPTION, field_schema_ref},
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

/// `true` for the `#[serde(other)]` catch-all unit variant.
pub(super) fn is_other_variant(variant: &syn::Variant) -> bool {
    extract_serde_skip_flags(&variant.attrs).other
}

/// `{}` for a variant whose content is written / read by a variant-level
/// `#[serde(with | serialize_with | deserialize_with = "..")]` on the side of
/// the view being built (its wire shape is unknown, see `wire_override`).
/// A unit variant has no content, so it never has one.
pub(super) fn variant_codec_schema(variant: &syn::Variant) -> Option<SchemaRef> {
    (!matches!(variant.fields, syn::Fields::Unit)
        && extract_field_codec(&variant.attrs).custom_for(current_side()))
    .then(|| {
        SchemaRef::Inline(Box::new(Schema {
            description: Some(CUSTOM_CODEC_DESCRIPTION.to_string()),
            ..Schema::default()
        }))
    })
}

/// Schema of the single field of a newtype variant on the current side.
pub(super) fn newtype_field_schema(
    field: &syn::Field,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    field_schema_ref(
        field,
        &extract_schema_constraints(&field.attrs),
        current_side(),
        known_schemas,
        struct_definitions,
    )
}

/// Build a schema for a variant's data (tuple or struct fields)
pub(super) fn build_variant_data_schema(
    variant: &syn::Variant,
    enum_rename_all_fields: Option<&str>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<SchemaRef> {
    let codec = variant_codec_schema(variant);
    if codec.is_some() {
        return codec;
    }
    match &variant.fields {
        syn::Fields::Unit => None,
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
            tuple_fields_schema(fields_unnamed, known_schemas, struct_definitions),
        ))),
        syn::Fields::Named(fields_named) => Some(SchemaRef::Inline(Box::new(
            build_struct_variant_fields(
                fields_named,
                enum_rename_all_fields,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(None, false),
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
            None,
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
}
