//! Code generation utilities for schema macros
//!
//! Provides functions to convert schema structures to `TokenStream` for code generation.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::type_utils::is_option_type;
use crate::{
    metadata::StructMetadata,
    parser::{
        extract_default, extract_field_rename, extract_rename_all, extract_skip,
        extract_skip_serializing_if, parse_type_to_schema_ref, rename_field, strip_raw_prefix,
    },
};

/// Generate Schema construction code with field filtering
#[allow(clippy::option_if_let_else)]
pub fn generate_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &std::collections::HashMap<String, StructMetadata>,
) -> TokenStream {
    let rename_all = extract_rename_all(&struct_item.attrs);

    // Build known_schemas and struct_definitions for type resolution
    let known_schemas: HashSet<String> = schema_storage.keys().cloned().collect();
    let struct_definitions: std::collections::HashMap<String, String> = schema_storage
        .values()
        .map(|s| (s.name.clone(), s.definition.clone()))
        .collect();

    let mut property_tokens = Vec::new();
    let mut required_fields = Vec::new();

    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            // Skip if serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            let rust_field_name = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix(&i.to_string()).to_string(),
            );

            // Apply rename
            let field_name = extract_field_rename(&field.attrs)
                .unwrap_or_else(|| rename_field(&rust_field_name, rename_all.as_deref()));

            // Apply omit filter (check both rust name and json name)
            if !omit_set.is_empty()
                && (omit_set.contains(&rust_field_name) || omit_set.contains(&field_name))
            {
                continue;
            }

            // Apply pick filter (check both rust name and json name)
            if !pick_set.is_empty()
                && !pick_set.contains(&rust_field_name)
                && !pick_set.contains(&field_name)
            {
                continue;
            }

            let field_type = &field.ty;

            // Generate schema for field type
            let schema_ref =
                parse_type_to_schema_ref(field_type, &known_schemas, &struct_definitions);
            let schema_ref_tokens = schema_ref_to_tokens(&schema_ref);

            property_tokens.push(quote! {
                properties.insert(#field_name.to_string(), #schema_ref_tokens);
            });

            // Check if field is required (not Option, no default, no skip_serializing_if)
            let has_default = extract_default(&field.attrs).is_some();
            let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);
            let is_optional = is_option_type(field_type);

            if !is_optional && !has_default && !has_skip_serializing_if {
                required_fields.push(field_name.clone());
            }
        }
    }

    let required_tokens = if required_fields.is_empty() {
        quote! { None }
    } else {
        let required_strs: Vec<&str> = required_fields
            .iter()
            .map(std::string::String::as_str)
            .collect();
        quote! { Some(vec![#(#required_strs.to_string()),*]) }
    };

    quote! {
        {
            let mut properties = std::collections::BTreeMap::new();
            #(#property_tokens)*
            vespera::schema::Schema {
                schema_type: Some(vespera::schema::SchemaType::Object),
                properties: if properties.is_empty() { None } else { Some(properties) },
                required: #required_tokens,
                ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
            }
        }
    }
}

/// Convert `SchemaRef` to `TokenStream` for code generation
pub fn schema_ref_to_tokens(schema_ref: &SchemaRef) -> TokenStream {
    match schema_ref {
        SchemaRef::Ref(reference) => {
            let ref_path = &reference.ref_path;
            quote! {
                vespera::schema::SchemaRef::Ref(vespera::schema::Reference::new(#ref_path.to_string()))
            }
        }
        SchemaRef::Inline(schema) => {
            let schema_tokens = schema_to_tokens(schema);
            quote! {
                vespera::schema::SchemaRef::Inline(Box::new(#schema_tokens))
            }
        }
    }
}

/// Convert Schema to `TokenStream` for code generation
#[allow(clippy::option_if_let_else)]
pub fn schema_to_tokens(schema: &Schema) -> TokenStream {
    let schema_type_tokens = match &schema.schema_type {
        Some(SchemaType::String) => quote! { Some(vespera::schema::SchemaType::String) },
        Some(SchemaType::Number) => quote! { Some(vespera::schema::SchemaType::Number) },
        Some(SchemaType::Integer) => quote! { Some(vespera::schema::SchemaType::Integer) },
        Some(SchemaType::Boolean) => quote! { Some(vespera::schema::SchemaType::Boolean) },
        Some(SchemaType::Array) => quote! { Some(vespera::schema::SchemaType::Array) },
        Some(SchemaType::Object) => quote! { Some(vespera::schema::SchemaType::Object) },
        Some(SchemaType::Null) => quote! { Some(vespera::schema::SchemaType::Null) },
        None => quote! { None },
    };

    let format_tokens = if let Some(f) = &schema.format {
        quote! { Some(#f.to_string()) }
    } else {
        quote! { None }
    };

    let nullable_tokens = match schema.nullable {
        Some(true) => quote! { Some(true) },
        Some(false) => quote! { Some(false) },
        None => quote! { None },
    };

    let ref_path_tokens = if let Some(rp) = &schema.ref_path {
        quote! { Some(#rp.to_string()) }
    } else {
        quote! { None }
    };

    let items_tokens = if let Some(items) = &schema.items {
        let inner = schema_ref_to_tokens(items);
        quote! { Some(Box::new(#inner)) }
    } else {
        quote! { None }
    };

    let properties_tokens = if let Some(props) = &schema.properties {
        let entries: Vec<_> = props
            .iter()
            .map(|(k, v)| {
                let v_tokens = schema_ref_to_tokens(v);
                quote! { (#k.to_string(), #v_tokens) }
            })
            .collect();
        quote! {
            Some({
                let mut map = std::collections::BTreeMap::new();
                #(map.insert(#entries.0, #entries.1);)*
                map
            })
        }
    } else {
        quote! { None }
    };

    let required_tokens = if let Some(req) = &schema.required {
        let req_strs: Vec<_> = req.iter().map(std::string::String::as_str).collect();
        quote! { Some(vec![#(#req_strs.to_string()),*]) }
    } else {
        quote! { None }
    };

    let minimum_tokens = if let Some(min) = schema.minimum {
        quote! { Some(#min) }
    } else {
        quote! { None }
    };

    let maximum_tokens = if let Some(max) = schema.maximum {
        quote! { Some(#max) }
    } else {
        quote! { None }
    };

    quote! {
        vespera::schema::Schema {
            ref_path: #ref_path_tokens,
            schema_type: #schema_type_tokens,
            format: #format_tokens,
            nullable: #nullable_tokens,
            items: #items_tokens,
            properties: #properties_tokens,
            required: #required_tokens,
            minimum: #minimum_tokens,
            maximum: #maximum_tokens,
            ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

    use super::*;

    #[test]
    fn test_generate_filtered_schema_empty_properties() {
        let struct_item: syn::ItemStruct = syn::parse_str("pub struct Empty {}").unwrap();
        let omit_set = HashSet::new();
        let pick_set = HashSet::new();
        let output = generate_filtered_schema(&struct_item, &omit_set, &pick_set, &HashMap::new())
            .to_string();
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_filtered_schema_with_default_field() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            pub struct WithDefault {
                #[serde(default)]
                pub field: String,
            }
        ",
        )
        .unwrap();
        let omit_set = HashSet::new();
        let pick_set = HashSet::new();
        let output = generate_filtered_schema(&struct_item, &omit_set, &pick_set, &HashMap::new())
            .to_string();
        assert!(output.contains("None"));
    }

    #[test]
    fn test_generate_filtered_schema_with_skip_serializing_if() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            pub struct WithSkip {
                #[serde(skip_serializing_if = "Option::is_none")]
                pub field: String,
            }
        "#,
        )
        .unwrap();
        let omit_set = HashSet::new();
        let pick_set = HashSet::new();
        let _output = generate_filtered_schema(&struct_item, &omit_set, &pick_set, &HashMap::new());
    }

    #[test]
    fn test_generate_filtered_schema_tuple_struct() {
        let struct_item: syn::ItemStruct =
            syn::parse_str("pub struct Tuple(i32, String);").unwrap();
        let omit_set = HashSet::new();
        let pick_set = HashSet::new();
        let _output = generate_filtered_schema(&struct_item, &omit_set, &pick_set, &HashMap::new());
    }

    #[test]
    fn test_schema_ref_to_tokens_ref_variant() {
        let schema_ref = SchemaRef::Ref(Reference::new("#/components/schemas/User".to_string()));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();
        assert!(output.contains("SchemaRef :: Ref"));
        assert!(output.contains("Reference :: new"));
    }

    #[test]
    fn test_schema_ref_to_tokens_inline_variant() {
        let schema = Schema::new(SchemaType::String);
        let schema_ref = SchemaRef::Inline(Box::new(schema));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();
        assert!(output.contains("SchemaRef :: Inline"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_string_type() {
        let schema = Schema::new(SchemaType::String);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: String"));
    }

    #[test]
    fn test_schema_to_tokens_integer_type() {
        let schema = Schema::new(SchemaType::Integer);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Integer"));
    }

    #[test]
    fn test_schema_to_tokens_number_type() {
        let schema = Schema::new(SchemaType::Number);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Number"));
    }

    #[test]
    fn test_schema_to_tokens_boolean_type() {
        let schema = Schema::new(SchemaType::Boolean);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Boolean"));
    }

    #[test]
    fn test_schema_to_tokens_array_type() {
        let schema = Schema::new(SchemaType::Array);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Array"));
    }

    #[test]
    fn test_schema_to_tokens_object_type() {
        let schema = Schema::new(SchemaType::Object);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Object"));
    }

    #[test]
    fn test_schema_to_tokens_null_type() {
        let schema = Schema::new(SchemaType::Null);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("SchemaType :: Null"));
    }

    #[test]
    fn test_schema_to_tokens_none_type() {
        let schema = Schema {
            schema_type: None,
            ..Default::default()
        };
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("schema_type : None"));
    }

    #[test]
    fn test_schema_to_tokens_with_format() {
        let mut schema = Schema::new(SchemaType::String);
        schema.format = Some("date-time".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("date-time"));
    }

    #[test]
    fn test_schema_to_tokens_with_nullable() {
        let mut schema = Schema::new(SchemaType::String);
        schema.nullable = Some(true);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("Some (true)"));
    }

    #[test]
    fn test_schema_to_tokens_nullable_false() {
        let mut schema = Schema::new(SchemaType::String);
        schema.nullable = Some(false);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("Some (false)"));
    }

    #[test]
    fn test_schema_to_tokens_with_ref_path() {
        let mut schema = Schema::new(SchemaType::Object);
        schema.ref_path = Some("#/components/schemas/User".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("#/components/schemas/User"));
    }

    #[test]
    fn test_schema_to_tokens_with_items() {
        let mut schema = Schema::new(SchemaType::Array);
        let item_schema = Schema::new(SchemaType::String);
        schema.items = Some(Box::new(SchemaRef::Inline(Box::new(item_schema))));
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("items"));
        assert!(output.contains("Some (Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_with_properties() {
        use std::collections::BTreeMap;

        let mut schema = Schema::new(SchemaType::Object);
        let mut props = BTreeMap::new();
        props.insert(
            "name".to_string(),
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::String))),
        );
        schema.properties = Some(props);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("properties"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_schema_to_tokens_with_required() {
        let mut schema = Schema::new(SchemaType::Object);
        schema.required = Some(vec!["id".to_string(), "name".to_string()]);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(output.contains("required"));
        assert!(output.contains("id"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_schema_to_tokens_with_minimum() {
        let mut schema = Schema::new(SchemaType::Integer);
        schema.minimum = Some(0.0);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(
            output.contains("minimum"),
            "should contain minimum: {output}"
        );
        assert!(output.contains("Some"), "should contain Some: {output}");
    }

    #[test]
    fn test_schema_to_tokens_with_maximum() {
        let mut schema = Schema::new(SchemaType::Integer);
        schema.maximum = Some(255.0);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();
        assert!(
            output.contains("maximum"),
            "should contain maximum: {output}"
        );
        assert!(output.contains("Some"), "should contain Some: {output}");
    }
}
