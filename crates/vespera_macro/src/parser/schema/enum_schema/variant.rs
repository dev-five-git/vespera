use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::SchemaRef;

use super::super::{
    serde_attrs::{extract_rename_all, extract_serde_skip_flags},
    struct_schema::{NamedFields, collect_named_fields, emitted_unnamed_types},
    type_schema::{parse_type_to_schema_ref, tuple_array_schema},
};

/// Build the fields of a struct variant through the SAME field walker structs
/// use, so `skip`, `flatten`, `rename`, `skip_serializing_if`, doc comments and
/// `#[schema(...)]` constraints behave identically on enum variants.
pub(super) fn build_struct_variant_fields(
    fields_named: &syn::FieldsNamed,
    enum_rename_all: Option<&str>,
    variant_attrs: &[syn::Attribute],
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> NamedFields {
    let variant_rename_all = extract_rename_all(variant_attrs);
    collect_named_fields(
        fields_named,
        variant_rename_all.as_deref().or(enum_rename_all),
        known_schemas,
        struct_definitions,
    )
}

/// `true` when a variant never appears in serialized output
/// (`#[serde(skip)]`, `#[serde(skip_serializing)]`), so it is left out of the
/// schema entirely.
pub(super) fn is_variant_omitted(variant: &syn::Variant) -> bool {
    let flags = extract_serde_skip_flags(&variant.attrs);
    flags.skip || flags.skip_serializing
}

/// `true` for the `#[serde(other)]` catch-all unit variant.
pub(super) fn is_other_variant(variant: &syn::Variant) -> bool {
    extract_serde_skip_flags(&variant.attrs).other
}

/// Build a schema for a variant's data (tuple or struct fields)
pub(super) fn build_variant_data_schema(
    variant: &syn::Variant,
    enum_rename_all: Option<&str>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Option<SchemaRef> {
    match &variant.fields {
        syn::Fields::Unit => None,
        syn::Fields::Unnamed(fields_unnamed) => {
            if fields_unnamed.unnamed.len() == 1 {
                // Single field tuple variant - just the inner type
                let inner_type = &fields_unnamed.unnamed[0].ty;
                Some(parse_type_to_schema_ref(
                    inner_type,
                    known_schemas,
                    struct_definitions,
                ))
            } else {
                // Multiple fields tuple variant - array with prefixItems over
                // the non-skipped fields
                Some(SchemaRef::Inline(Box::new(tuple_array_schema(
                    emitted_unnamed_types(fields_unnamed).into_iter(),
                    known_schemas,
                    struct_definitions,
                ))))
            }
        }
        syn::Fields::Named(fields_named) => Some(SchemaRef::Inline(Box::new(
            build_struct_variant_fields(
                fields_named,
                enum_rename_all,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(None),
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
