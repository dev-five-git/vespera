use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::super::{
    serde_attrs::{
        extract_doc_comment, extract_field_rename, extract_rename_all, rename_field,
        strip_raw_prefix_owned,
    },
    type_schema::parse_type_to_schema_ref,
};
use crate::schema_macro::type_utils::is_option_type;

/// Build properties for a struct variant's fields
pub(super) fn build_struct_variant_properties(
    fields_named: &syn::FieldsNamed,
    enum_rename_all: Option<&str>,
    variant_attrs: &[syn::Attribute],
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> (BTreeMap<String, SchemaRef>, Vec<String>) {
    let mut variant_properties = BTreeMap::new();
    let mut variant_required = Vec::with_capacity(fields_named.named.len());
    let variant_rename_all = extract_rename_all(variant_attrs);

    for field in &fields_named.named {
        let rust_field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |i| strip_raw_prefix_owned(i.to_string()),
        );

        // Check for field-level rename attribute first (takes precedence)
        let field_name = extract_field_rename(&field.attrs).unwrap_or_else(|| {
            rename_field(
                &rust_field_name,
                variant_rename_all.as_deref().or(enum_rename_all),
            )
        });

        let field_type = &field.ty;
        let mut schema_ref =
            parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

        // Extract doc comment from field and set as description
        if let Some(doc) = extract_doc_comment(&field.attrs) {
            match &mut schema_ref {
                SchemaRef::Inline(schema) => {
                    schema.description = Some(doc);
                }
                SchemaRef::Ref(_) => {
                    let ref_schema = std::mem::replace(
                        &mut schema_ref,
                        SchemaRef::Inline(Box::new(Schema::object())),
                    );
                    if let SchemaRef::Ref(reference) = ref_schema {
                        schema_ref = SchemaRef::Inline(Box::new(Schema {
                            description: Some(doc),
                            all_of: Some(vec![SchemaRef::Ref(reference)]),
                            ..Default::default()
                        }));
                    }
                }
            }
        }

        variant_properties.insert(field_name.clone(), schema_ref);

        // Check if field is Option<T>
        let is_optional = is_option_type(field_type);

        if !is_optional {
            variant_required.push(field_name);
        }
    }

    (variant_properties, variant_required)
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
                // Multiple fields tuple variant - array with prefixItems
                let mut tuple_item_schemas = Vec::with_capacity(fields_unnamed.unnamed.len());
                for field in &fields_unnamed.unnamed {
                    let field_schema =
                        parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions);
                    tuple_item_schemas.push(field_schema);
                }

                let tuple_len = tuple_item_schemas.len();
                Some(SchemaRef::Inline(Box::new(Schema {
                    prefix_items: Some(tuple_item_schemas),
                    min_items: Some(tuple_len),
                    max_items: Some(tuple_len),
                    items: None,
                    ..Schema::new(SchemaType::Array)
                })))
            }
        }
        syn::Fields::Named(fields_named) => {
            let (properties, required) = build_struct_variant_properties(
                fields_named,
                enum_rename_all,
                &variant.attrs,
                known_schemas,
                struct_definitions,
            );

            Some(SchemaRef::Inline(Box::new(Schema {
                properties: if properties.is_empty() {
                    None
                } else {
                    Some(properties)
                },
                required: if required.is_empty() {
                    None
                } else {
                    Some(required)
                },
                ..Schema::object()
            })))
        }
    }
}
