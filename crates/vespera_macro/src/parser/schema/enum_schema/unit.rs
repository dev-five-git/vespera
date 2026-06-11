use vespera_core::schema::{Schema, SchemaType};

use super::super::serde_attrs::{extract_field_rename, rename_field, strip_raw_prefix_owned};

/// Parse a simple enum (all unit variants) to a string schema with enum values.
pub(super) fn parse_unit_enum_to_schema(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
) -> Schema {
    let mut enum_values = Vec::with_capacity(enum_item.variants.len());

    for variant in &enum_item.variants {
        let variant_name = strip_raw_prefix_owned(variant.ident.to_string());

        // Check for variant-level rename attribute first (takes precedence)
        let enum_value = extract_field_rename(&variant.attrs)
            .unwrap_or_else(|| rename_field(&variant_name, rename_all));

        enum_values.push(serde_json::Value::String(enum_value));
    }

    Schema {
        schema_type: Some(SchemaType::String),
        description,
        r#enum: if enum_values.is_empty() {
            None
        } else {
            Some(enum_values)
        },
        ..Schema::string()
    }
}

/// Get the variant key (name after rename transformations)
pub(super) fn get_variant_key(variant: &syn::Variant, rename_all: Option<&str>) -> String {
    let variant_name = strip_raw_prefix_owned(variant.ident.to_string());

    extract_field_rename(&variant.attrs).unwrap_or_else(|| rename_field(&variant_name, rename_all))
}
