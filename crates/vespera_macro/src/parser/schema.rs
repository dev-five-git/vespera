use std::collections::{BTreeMap, HashMap};

use syn::{Fields, Type};
use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

pub fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Parse the attribute tokens manually
            // Format: #[serde(rename_all = "camelCase")]
            let tokens = attr.meta.require_list().ok()?;
            let token_str = tokens.tokens.to_string();

            // Look for rename_all = "..." pattern
            if let Some(start) = token_str.find("rename_all") {
                let remaining = &token_str[start + "rename_all".len()..];
                if let Some(equals_pos) = remaining.find('=') {
                    let value_part = &remaining[equals_pos + 1..].trim();
                    // Extract string value (remove quotes)
                    if value_part.starts_with('"') && value_part.ends_with('"') {
                        let value = &value_part[1..value_part.len() - 1];
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn extract_field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            // Use parse_nested_meta to parse nested attributes
            let mut found_rename = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename = Some(s.value());
                }
                Ok(())
            });
            if let Some(rename_value) = found_rename {
                return Some(rename_value);
            }

            // Fallback: manual token parsing with regex-like approach
            let tokens = meta_list.tokens.to_string();
            // Look for pattern: rename = "value" (with proper word boundaries)
            if let Some(start) = tokens.find("rename") {
                // Avoid false positives from rename_all
                if tokens[start..].starts_with("rename_all") {
                    continue;
                }
                // Check that "rename" is a standalone word (not part of another word)
                let before = if start > 0 { &tokens[..start] } else { "" };
                let after_start = start + "rename".len();
                let after = if after_start < tokens.len() {
                    &tokens[after_start..]
                } else {
                    ""
                };

                let before_char = before.chars().last().unwrap_or(' ');
                let after_char = after.chars().next().unwrap_or(' ');

                // Check if rename is a standalone word (preceded by space/comma/paren, followed by space/equals)
                if (before_char == ' ' || before_char == ',' || before_char == '(')
                    && (after_char == ' ' || after_char == '=')
                {
                    // Find the equals sign and extract the quoted value
                    if let Some(equals_pos) = after.find('=') {
                        let value_part = &after[equals_pos + 1..].trim();
                        // Extract string value (remove quotes)
                        if let Some(quote_start) = value_part.find('"') {
                            let after_quote = &value_part[quote_start + 1..];
                            if let Some(quote_end) = after_quote.find('"') {
                                let value = &after_quote[..quote_end];
                                return Some(value.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract skip attribute from field attributes
/// Returns true if #[serde(skip)] is present
pub(super) fn extract_skip(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let tokens = meta_list.tokens.to_string();
            // Check for "skip" (not part of skip_serializing_if or skip_deserializing)
            if tokens.contains("skip") {
                // Make sure it's not skip_serializing_if or skip_deserializing
                if !tokens.contains("skip_serializing_if") && !tokens.contains("skip_deserializing")
                {
                    // Check if it's a standalone "skip"
                    let skip_pos = tokens.find("skip");
                    if let Some(pos) = skip_pos {
                        let before = if pos > 0 { &tokens[..pos] } else { "" };
                        let after = &tokens[pos + "skip".len()..];
                        // Check if skip is not part of another word
                        let before_char = before.chars().last().unwrap_or(' ');
                        let after_char = after.chars().next().unwrap_or(' ');
                        if (before_char == ' ' || before_char == ',' || before_char == '(')
                            && (after_char == ' ' || after_char == ',' || after_char == ')')
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Extract skip_serializing_if attribute from field attributes
/// Returns true if #[serde(skip_serializing_if = "...")] is present
pub fn extract_skip_serializing_if(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip_serializing_if") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: check tokens string
            let tokens = meta_list.tokens.to_string();
            if tokens.contains("skip_serializing_if") {
                return true;
            }
        }
    }
    false
}

/// Extract default attribute from field attributes
/// Returns:
/// - Some(None) if #[serde(default)] is present (no function)
/// - Some(Some(function_name)) if #[serde(default = "function_name")] is present
/// - None if no default attribute is present
pub fn extract_default(attrs: &[syn::Attribute]) -> Option<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found_default: Option<Option<String>> = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    // Check if it has a value (default = "function_name")
                    if let Ok(value) = meta.value() {
                        if let Ok(syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        })) = value.parse::<syn::Expr>()
                        {
                            found_default = Some(Some(s.value()));
                        }
                    } else {
                        // Just "default" without value
                        found_default = Some(None);
                    }
                }
                Ok(())
            });
            if let Some(default_value) = found_default {
                return Some(default_value);
            }

            // Fallback: manual token parsing
            let tokens = meta_list.tokens.to_string();
            if let Some(start) = tokens.find("default") {
                let remaining = &tokens[start + "default".len()..];
                if remaining.trim_start().starts_with('=') {
                    // default = "function_name"
                    let value_part = remaining.trim_start()[1..].trim();
                    if value_part.starts_with('"') && value_part.ends_with('"') {
                        let function_name = &value_part[1..value_part.len() - 1];
                        return Some(Some(function_name.to_string()));
                    }
                } else {
                    // Just "default" without = (standalone)
                    let before = if start > 0 { &tokens[..start] } else { "" };
                    let after = &remaining;
                    let before_char = before.chars().last().unwrap_or(' ');
                    let after_char = after.chars().next().unwrap_or(' ');
                    if (before_char == ' ' || before_char == ',' || before_char == '(')
                        && (after_char == ' ' || after_char == ',' || after_char == ')')
                    {
                        return Some(None);
                    }
                }
            }
        }
    }
    None
}

pub fn rename_field(field_name: &str, rename_all: Option<&str>) -> String {
    // "lowercase", "UPPERCASE", "PascalCase", "camelCase", "snake_case", "SCREAMING_SNAKE_CASE", "kebab-case", "SCREAMING-KEBAB-CASE"
    match rename_all {
        Some("camelCase") => {
            // Convert snake_case to camelCase
            let mut result = String::new();
            let mut capitalize_next = false;
            for ch in field_name.chars() {
                if ch == '_' {
                    capitalize_next = true;
                } else if capitalize_next {
                    result.push(ch.to_uppercase().next().unwrap_or(ch));
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
            }
            result
        }
        Some("snake_case") => {
            // Convert camelCase to snake_case
            let mut result = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() && i > 0 {
                    result.push('_');
                }
                result.push(ch.to_lowercase().next().unwrap_or(ch));
            }
            result
        }
        Some("kebab-case") => {
            // Convert snake_case or Camel/PascalCase to kebab-case (lowercase with hyphens)
            let mut result = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() {
                    if i > 0 && !result.ends_with('-') {
                        result.push('-');
                    }
                    result.push(ch.to_lowercase().next().unwrap_or(ch));
                } else if ch == '_' {
                    result.push('-');
                } else {
                    result.push(ch);
                }
            }
            result
        }
        Some("PascalCase") => {
            // Convert snake_case to PascalCase
            let mut result = String::new();
            let mut capitalize_next = true;
            for ch in field_name.chars() {
                if ch == '_' {
                    capitalize_next = true;
                } else if capitalize_next {
                    result.push(ch.to_uppercase().next().unwrap_or(ch));
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
            }
            result
        }
        Some("lowercase") => {
            // Convert to lowercase
            field_name.to_lowercase()
        }
        Some("UPPERCASE") => {
            // Convert to UPPERCASE
            field_name.to_uppercase()
        }
        Some("SCREAMING_SNAKE_CASE") => {
            // Convert to SCREAMING_SNAKE_CASE
            // If already in SCREAMING_SNAKE_CASE format, return as is
            if field_name.chars().all(|c| c.is_uppercase() || c == '_') && field_name.contains('_')
            {
                return field_name.to_string();
            }
            // First convert to snake_case if needed, then uppercase
            let mut snake_case = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() && i > 0 && !snake_case.ends_with('_') {
                    snake_case.push('_');
                }
                if ch != '_' && ch != '-' {
                    snake_case.push(ch.to_lowercase().next().unwrap_or(ch));
                } else if ch == '_' {
                    snake_case.push('_');
                }
            }
            snake_case.to_uppercase()
        }
        Some("SCREAMING-KEBAB-CASE") => {
            // Convert to SCREAMING-KEBAB-CASE
            // First convert to kebab-case if needed, then uppercase
            let mut kebab_case = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase()
                    && i > 0
                    && !kebab_case.ends_with('-')
                    && !kebab_case.ends_with('_')
                {
                    kebab_case.push('-');
                }
                if ch == '_' {
                    kebab_case.push('-');
                } else if ch != '-' {
                    kebab_case.push(ch.to_lowercase().next().unwrap_or(ch));
                } else {
                    kebab_case.push('-');
                }
            }
            kebab_case.to_uppercase()
        }
        _ => field_name.to_string(),
    }
}

pub fn parse_enum_to_schema(
    enum_item: &syn::ItemEnum,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    // Extract rename_all attribute from enum
    let rename_all = extract_rename_all(&enum_item.attrs);

    // Check if all variants are unit variants
    let all_unit = enum_item
        .variants
        .iter()
        .all(|v| matches!(v.fields, syn::Fields::Unit));

    if all_unit {
        // Simple enum with string values
        let mut enum_values = Vec::new();

        for variant in &enum_item.variants {
            let variant_name = variant.ident.to_string();

            // Check for variant-level rename attribute first (takes precedence)
            let enum_value = if let Some(renamed) = extract_field_rename(&variant.attrs) {
                renamed
            } else {
                // Apply rename_all transformation if present
                rename_field(&variant_name, rename_all.as_deref())
            };

            enum_values.push(serde_json::Value::String(enum_value));
        }

        Schema {
            schema_type: Some(SchemaType::String),
            r#enum: if enum_values.is_empty() {
                None
            } else {
                Some(enum_values)
            },
            ..Schema::string()
        }
    } else {
        // Enum with data - use oneOf
        let mut one_of_schemas = Vec::new();

        for variant in &enum_item.variants {
            let variant_name = variant.ident.to_string();

            // Check for variant-level rename attribute first (takes precedence)
            let variant_key = if let Some(renamed) = extract_field_rename(&variant.attrs) {
                renamed
            } else {
                // Apply rename_all transformation if present
                rename_field(&variant_name, rename_all.as_deref())
            };

            let variant_schema = match &variant.fields {
                syn::Fields::Unit => {
                    // Unit variant: {"const": "VariantName"}
                    Schema {
                        r#enum: Some(vec![serde_json::Value::String(variant_key)]),
                        ..Schema::string()
                    }
                }
                syn::Fields::Unnamed(fields_unnamed) => {
                    // Tuple variant: {"VariantName": <inner_type>}
                    // For single field: {"VariantName": <type>}
                    // For multiple fields: {"VariantName": [<type1>, <type2>, ...]}
                    if fields_unnamed.unnamed.len() == 1 {
                        // Single field tuple variant
                        let inner_type = &fields_unnamed.unnamed[0].ty;
                        let inner_schema =
                            parse_type_to_schema_ref(inner_type, known_schemas, struct_definitions);

                        let mut properties = BTreeMap::new();
                        properties.insert(variant_key.clone(), inner_schema);

                        Schema {
                            properties: Some(properties),
                            required: Some(vec![variant_key]),
                            ..Schema::object()
                        }
                    } else {
                        // Multiple fields tuple variant - serialize as array
                        // serde serializes tuple variants as: {"VariantName": [value1, value2, ...]}
                        // For OpenAPI 3.1, we use prefixItems to represent tuple arrays
                        let mut tuple_item_schemas = Vec::new();
                        for field in &fields_unnamed.unnamed {
                            let field_schema = parse_type_to_schema_ref(
                                &field.ty,
                                known_schemas,
                                struct_definitions,
                            );
                            tuple_item_schemas.push(field_schema);
                        }

                        let tuple_len = tuple_item_schemas.len();

                        // Create array schema with prefixItems for tuple arrays (OpenAPI 3.1)
                        let array_schema = Schema {
                            prefix_items: Some(tuple_item_schemas),
                            min_items: Some(tuple_len),
                            max_items: Some(tuple_len),
                            items: None, // Do not use prefixItems and items together
                            ..Schema::new(SchemaType::Array)
                        };

                        let mut properties = BTreeMap::new();
                        properties.insert(
                            variant_key.clone(),
                            SchemaRef::Inline(Box::new(array_schema)),
                        );

                        Schema {
                            properties: Some(properties),
                            required: Some(vec![variant_key]),
                            ..Schema::object()
                        }
                    }
                }
                syn::Fields::Named(fields_named) => {
                    // Struct variant: {"VariantName": {field1: type1, field2: type2, ...}}
                    let mut variant_properties = BTreeMap::new();
                    let mut variant_required = Vec::new();
                    let variant_rename_all = extract_rename_all(&variant.attrs);

                    for field in &fields_named.named {
                        let rust_field_name = field
                            .ident
                            .as_ref()
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| "unknown".to_string());

                        // Check for field-level rename attribute first (takes precedence)
                        let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                            renamed
                        } else {
                            // Apply rename_all transformation if present
                            rename_field(
                                &rust_field_name,
                                variant_rename_all.as_deref().or(rename_all.as_deref()),
                            )
                        };

                        let field_type = &field.ty;
                        let schema_ref =
                            parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                        variant_properties.insert(field_name.clone(), schema_ref);

                        // Check if field is Option<T>
                        let is_optional = matches!(
                            field_type,
                            Type::Path(type_path)
                                if type_path
                                    .path
                                    .segments
                                    .first()
                                    .map(|s| s.ident == "Option")
                                    .unwrap_or(false)
                        );

                        if !is_optional {
                            variant_required.push(field_name);
                        }
                    }

                    // Wrap struct variant in an object with the variant name as key
                    let inner_struct_schema = Schema {
                        properties: if variant_properties.is_empty() {
                            None
                        } else {
                            Some(variant_properties)
                        },
                        required: if variant_required.is_empty() {
                            None
                        } else {
                            Some(variant_required)
                        },
                        ..Schema::object()
                    };

                    let mut properties = BTreeMap::new();
                    properties.insert(
                        variant_key.clone(),
                        SchemaRef::Inline(Box::new(inner_struct_schema)),
                    );

                    Schema {
                        properties: Some(properties),
                        required: Some(vec![variant_key]),
                        ..Schema::object()
                    }
                }
            };

            one_of_schemas.push(SchemaRef::Inline(Box::new(variant_schema)));
        }

        Schema {
            schema_type: None, // oneOf doesn't have a single type
            one_of: if one_of_schemas.is_empty() {
                None
            } else {
                Some(one_of_schemas)
            },
            ..Schema::new(SchemaType::Object)
        }
    }
}

pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Schema {
    let mut properties = BTreeMap::new();
    let mut required = Vec::new();

    // Extract rename_all attribute from struct
    let rename_all = extract_rename_all(&struct_item.attrs);

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            for field in &fields_named.named {
                // Check if field should be skipped
                if extract_skip(&field.attrs) {
                    continue;
                }

                let rust_field_name = field
                    .ident
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                // Check for field-level rename attribute first (takes precedence)
                let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                    renamed
                } else {
                    // Apply rename_all transformation if present
                    rename_field(&rust_field_name, rename_all.as_deref())
                };

                let field_type = &field.ty;

                let mut schema_ref =
                    parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                // Check for default attribute
                let has_default = extract_default(&field.attrs).is_some();

                // Check for skip_serializing_if attribute
                let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);

                // If default or skip_serializing_if is present, mark field as optional (not required)
                // and set default value if it's a simple default (not a function)
                if has_default || has_skip_serializing_if {
                    // For default = "function_name", we'll handle it in openapi_generator
                    // For now, just mark as optional
                    if let SchemaRef::Inline(ref mut _schema) = schema_ref {
                        // Default will be set later in openapi_generator if it's a function
                        // For simple default, we could set it here, but serde handles it
                    }
                } else {
                    // Check if field is Option<T>
                    let is_optional = matches!(
                        field_type,
                        Type::Path(type_path)
                            if type_path
                                .path
                                .segments
                                .first()
                                .map(|s| s.ident == "Option")
                                .unwrap_or(false)
                    );

                    if !is_optional {
                        required.push(field_name.clone());
                    }
                }

                properties.insert(field_name, schema_ref);
            }
        }
        Fields::Unnamed(_) => {
            // Tuple structs are not supported for now
        }
        Fields::Unit => {
            // Unit structs have no fields
        }
    }

    Schema {
        schema_type: Some(SchemaType::Object),
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
    }
}

fn substitute_type(ty: &Type, generic_params: &[String], concrete_types: &[&Type]) -> Type {
    // Check if this is a generic parameter
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        let ident_str = segment.ident.to_string();
        if generic_params.contains(&ident_str) && segment.arguments.is_none() {
            // Find the index and substitute
            if let Some(index) = generic_params.iter().position(|p| p == &ident_str)
                && let Some(concrete_ty) = concrete_types.get(index)
            {
                return (*concrete_ty).clone();
            }
        }
    }

    // For complex types, use quote! to regenerate with substitutions
    let tokens = quote::quote! { #ty };
    let mut new_tokens = tokens.to_string();

    // Replace generic parameter names with concrete types
    for (param, concrete_ty) in generic_params.iter().zip(concrete_types.iter()) {
        // Replace standalone generic parameter (not part of another identifier)
        let pattern = format!(r"\b{}\b", param);
        let replacement = quote::quote! { #concrete_ty }.to_string();
        new_tokens = new_tokens.replace(&pattern, &replacement);
    }

    // Parse the substituted type
    syn::parse_str::<Type>(&new_tokens).unwrap_or_else(|_| ty.clone())
}

pub(super) fn is_primitive_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.len() == 1 {
                let ident = path.segments[0].ident.to_string();
                matches!(
                    ident.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "String"
                        | "str"
                )
            } else {
                false
            }
        }
        _ => false,
    }
}

pub fn parse_type_to_schema_ref(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    parse_type_to_schema_ref_with_schemas(ty, known_schemas, struct_definitions)
}

pub(super) fn parse_type_to_schema_ref_with_schemas(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.is_empty() {
                return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
            }

            // Get the last segment as the type name (handles paths like crate::TestStruct)
            let segment = path.segments.last().unwrap();
            let ident_str = segment.ident.to_string();

            // Handle generic types
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                match ident_str.as_str() {
                    "Vec" | "Option" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner_schema = parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            if ident_str == "Vec" {
                                return SchemaRef::Inline(Box::new(Schema::array(inner_schema)));
                            } else {
                                // Option<T> -> nullable schema
                                match inner_schema {
                                    SchemaRef::Inline(mut schema) => {
                                        schema.nullable = Some(true);
                                        return SchemaRef::Inline(schema);
                                    }
                                    SchemaRef::Ref(reference) => {
                                        // Wrap reference in an inline schema to attach nullable flag
                                        return SchemaRef::Inline(Box::new(Schema {
                                            ref_path: Some(reference.ref_path),
                                            schema_type: None,
                                            nullable: Some(true),
                                            ..Schema::new(SchemaType::Object)
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    "HashMap" | "BTreeMap" => {
                        // HashMap<K, V> or BTreeMap<K, V> -> object with additionalProperties
                        // K is typically String, we use V as the value type
                        if args.args.len() >= 2
                            && let (
                                Some(syn::GenericArgument::Type(_key_ty)),
                                Some(syn::GenericArgument::Type(value_ty)),
                            ) = (args.args.get(0), args.args.get(1))
                        {
                            let value_schema = parse_type_to_schema_ref(
                                value_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            // Convert SchemaRef to serde_json::Value for additional_properties
                            let additional_props_value = match value_schema {
                                SchemaRef::Ref(ref_ref) => {
                                    serde_json::json!({ "$ref": ref_ref.ref_path })
                                }
                                SchemaRef::Inline(schema) => {
                                    serde_json::to_value(&*schema).unwrap_or(serde_json::json!({}))
                                }
                            };
                            return SchemaRef::Inline(Box::new(Schema {
                                schema_type: Some(SchemaType::Object),
                                additional_properties: Some(additional_props_value),
                                ..Schema::object()
                            }));
                        }
                    }
                    _ => {}
                }
            }

            // Handle primitive types
            match ident_str.as_str() {
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => {
                    SchemaRef::Inline(Box::new(Schema::integer()))
                }
                "f32" | "f64" => SchemaRef::Inline(Box::new(Schema::number())),
                "bool" => SchemaRef::Inline(Box::new(Schema::boolean())),
                "String" | "str" => SchemaRef::Inline(Box::new(Schema::string())),
                // Standard library types that should not be referenced
                // Note: HashMap and BTreeMap are handled above in generic types
                "Vec" | "Option" | "Result" | "Json" | "Path" | "Query" | "Header" => {
                    // These are not schema types, return object schema
                    SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                }
                _ => {
                    // Check if this is a known schema (struct with Schema derive)
                    // Try both the full path and just the type name
                    let type_name = if path.segments.len() > 1 {
                        // For paths like crate::TestStruct, use just the type name
                        ident_str.clone()
                    } else {
                        ident_str.clone()
                    };

                    if known_schemas.contains_key(&type_name) {
                        // Check if this is a generic type with type parameters
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            // This is a concrete generic type like GenericStruct<String>
                            // Inline the schema by substituting generic parameters with concrete types
                            if let Some(base_def) = struct_definitions.get(&type_name)
                                && let Ok(mut parsed) = syn::parse_str::<syn::ItemStruct>(base_def)
                            {
                                // Extract generic parameter names from the struct definition
                                let generic_params: Vec<String> = parsed
                                    .generics
                                    .params
                                    .iter()
                                    .filter_map(|param| {
                                        if let syn::GenericParam::Type(type_param) = param {
                                            Some(type_param.ident.to_string())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();

                                // Extract concrete type arguments
                                let concrete_types: Vec<&Type> = args
                                    .args
                                    .iter()
                                    .filter_map(|arg| {
                                        if let syn::GenericArgument::Type(ty) = arg {
                                            Some(ty)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();

                                // Substitute generic parameters with concrete types in all fields
                                if generic_params.len() == concrete_types.len() {
                                    if let syn::Fields::Named(fields_named) = &mut parsed.fields {
                                        for field in &mut fields_named.named {
                                            field.ty = substitute_type(
                                                &field.ty,
                                                &generic_params,
                                                &concrete_types,
                                            );
                                        }
                                    }

                                    // Remove generics from the struct (it's now concrete)
                                    parsed.generics.params.clear();
                                    parsed.generics.where_clause = None;

                                    // Parse the substituted struct to schema (inline)
                                    let schema = parse_struct_to_schema(
                                        &parsed,
                                        known_schemas,
                                        struct_definitions,
                                    );
                                    return SchemaRef::Inline(Box::new(schema));
                                }
                            }
                        }
                        // Non-generic type or generic without parameters - use reference
                        SchemaRef::Ref(Reference::schema(&type_name))
                    } else {
                        // For unknown custom types, return object schema instead of reference
                        // This prevents creating invalid references to non-existent schemas
                        SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                    }
                }
            }
        }
        Type::Reference(type_ref) => {
            // Handle &T, &mut T, etc.
            parse_type_to_schema_ref_with_schemas(&type_ref.elem, known_schemas, struct_definitions)
        }
        _ => SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;
    use std::collections::HashMap;
    use vespera_core::schema::{SchemaRef, SchemaType};

    #[rstest]
    #[case("HashMap<String, i32>", Some(SchemaType::Object), true)]
    #[case("Option<String>", Some(SchemaType::String), false)] // nullable check
    fn test_parse_type_to_schema_ref_cases(
        #[case] ty_src: &str,
        #[case] expected_type: Option<SchemaType>,
        #[case] expect_additional_props: bool,
    ) {
        let ty: syn::Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashMap::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, expected_type);
            if expect_additional_props {
                assert!(schema.additional_properties.is_some());
            }
            if ty_src.starts_with("Option") {
                assert_eq!(schema.nullable, Some(true));
            }
        } else {
            panic!("Expected inline schema for {}", ty_src);
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_option_ref_nullable() {
        let mut known = HashMap::new();
        known.insert("User".to_string(), "struct User;".to_string());

        let ty: syn::Type = syn::parse_str("Option<User>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());

        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(
                    schema.ref_path,
                    Some("#/components/schemas/User".to_string())
                );
                assert_eq!(schema.nullable, Some(true));
                assert_eq!(schema.schema_type, None);
            }
            _ => panic!("Expected inline schema for Option<User>"),
        }
    }

    #[rstest]
    #[case(
        r#"
        #[serde(rename_all = "kebab-case")]
        enum Status {
            #[serde(rename = "ok-status")]
            Ok,
            ErrorCode,
        }
        "#,
        SchemaType::String,
        vec!["ok-status", "error-code"], // rename_all is not applied in this path
        "status"
    )]
    #[case(
        r#"
        enum Simple {
            First,
            Second,
        }
        "#,
        SchemaType::String,
        vec!["First", "Second"],
        "simple"
    )]
    #[case(
        r#"
        #[serde(rename_all = "snake_case")]
        enum Simple {
            FirstItem,
            SecondItem,
        }
        "#,
        SchemaType::String,
        vec!["first_item", "second_item"],
        "simple_snake"
    )]
    fn test_parse_enum_to_schema_unit_variants(
        #[case] enum_src: &str,
        #[case] expected_type: SchemaType,
        #[case] expected_enum: Vec<&str>,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        assert_eq!(schema.schema_type, Some(expected_type));
        let got = schema
            .clone()
            .r#enum
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(got, expected_enum);
        with_settings!({ snapshot_suffix => format!("unit_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r#"
        enum Event {
            Data(String),
        }
        "#,
        1,
        Some(SchemaType::String),
        0, // single-field tuple variant stored as object with inline schema
        "tuple_single"
    )]
    #[case(
        r#"
        enum Pair {
            Values(i32, String),
        }
        "#,
        1,
        Some(SchemaType::Array),
        2, // tuple array prefix_items length
        "tuple_multi"
    )]
    #[case(
        r#"
        enum Msg {
            Detail { id: i32, note: Option<String> },
        }
        "#,
        1,
        Some(SchemaType::Object),
        0, // not an array; ignore prefix_items length
        "named_object"
    )]
    fn test_parse_enum_to_schema_tuple_and_named_variants(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_inner_type: Option<SchemaType>,
        #[case] expected_prefix_items_len: usize,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.clone().one_of.expect("one_of missing");
        assert_eq!(one_of.len(), expected_one_of_len);

        if let Some(inner_expected) = expected_inner_type.clone() {
            if let SchemaRef::Inline(obj) = &one_of[0] {
                let props = obj.properties.as_ref().expect("props missing");
                // take first property value
                let inner_schema = props.values().next().expect("no property value");
                match inner_expected {
                    SchemaType::Array => {
                        if let SchemaRef::Inline(array_schema) = inner_schema {
                            assert_eq!(array_schema.schema_type, Some(SchemaType::Array));
                            if expected_prefix_items_len > 0 {
                                assert_eq!(
                                    array_schema.prefix_items.as_ref().unwrap().len(),
                                    expected_prefix_items_len
                                );
                            }
                        } else {
                            panic!("Expected inline array schema");
                        }
                    }
                    SchemaType::Object => {
                        if let SchemaRef::Inline(inner_obj) = inner_schema {
                            assert_eq!(inner_obj.schema_type, Some(SchemaType::Object));
                            let inner_props = inner_obj.properties.as_ref().unwrap();
                            assert!(inner_props.contains_key("id"));
                            assert!(inner_props.contains_key("note"));
                            assert!(
                                inner_obj
                                    .required
                                    .as_ref()
                                    .unwrap()
                                    .contains(&"id".to_string())
                            );
                        } else {
                            panic!("Expected inline object schema");
                        }
                    }
                    _ => {}
                }
            } else {
                panic!("Expected inline schema in one_of");
            }
        }

        with_settings!({ snapshot_suffix => format!("tuple_named_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r#"
        enum Mixed {
            Ready,
            Data(String),
        }
        "#,
        2,
        SchemaType::String,
        "Ready"
    )]
    fn test_parse_enum_to_schema_mixed_unit_variant(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_unit_type: SchemaType,
        #[case] expected_unit_value: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing for mixed enum");
        assert_eq!(one_of.len(), expected_one_of_len);

        let unit_schema = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema for unit variant"),
        };
        assert_eq!(unit_schema.schema_type, Some(expected_unit_type));
        let unit_enum = unit_schema.r#enum.as_ref().expect("enum values missing");
        assert_eq!(unit_enum[0].as_str().unwrap(), expected_unit_value);
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_for_data_variant() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "kebab-case")]
            enum Payload {
                DataItem(String),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("data-item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_uses_enum_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Event {
                Detail { UserId: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let inner = match props.get("detail").expect("variant key missing") {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline inner schema"),
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        assert!(inner_props.contains_key("user_id"));
        assert!(!inner_props.contains_key("UserId"));
    }

    #[test]
    fn test_parse_enum_to_schema_variant_rename_overrides_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename = "Explicit")]
                DataItem(i32),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("Explicit"));
        assert!(!props.contains_key("data_item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_rename_overrides_variant_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename_all = "kebab-case")]
                Detail { #[serde(rename = "ID")] user_id: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(&enum_item, &HashMap::new(), &HashMap::new());
        let one_of = schema.one_of.expect("one_of missing");
        let variant_obj = match &one_of[0] {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline schema"),
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let inner = match props
            .get("detail")
            .or_else(|| props.get("Detail"))
            .expect("variant key missing")
        {
            SchemaRef::Inline(s) => s,
            _ => panic!("Expected inline inner schema"),
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        assert!(inner_props.contains_key("ID")); // field-level rename wins
        assert!(!inner_props.contains_key("user-id")); // variant rename_all ignored for this field
    }

    #[test]
    fn test_parse_struct_to_schema_required_optional() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct User {
                id: i32,
                name: Option<String>,
            }
        "#,
        )
        .unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(
            schema
                .required
                .as_ref()
                .unwrap()
                .contains(&"id".to_string())
        );
        assert!(
            !schema
                .required
                .as_ref()
                .unwrap()
                .contains(&"name".to_string())
        );
    }

    #[test]
    fn test_parse_struct_to_schema_rename_all_and_field_rename() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase")]
            struct Profile {
                #[serde(rename = "id")]
                user_id: i32,
                display_name: Option<String>,
            }
        "#,
        )
        .unwrap();

        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        let props = schema.properties.as_ref().expect("props missing");
        assert!(props.contains_key("id")); // field-level rename wins
        assert!(props.contains_key("displayName")); // rename_all applied
        let required = schema.required.as_ref().expect("required missing");
        assert!(required.contains(&"id".to_string()));
        assert!(!required.contains(&"displayName".to_string())); // Option makes it optional
    }

    #[rstest]
    #[case("struct Wrapper(i32);")]
    #[case("struct Empty;")]
    fn test_parse_struct_to_schema_tuple_and_unit_structs(#[case] struct_src: &str) {
        let struct_item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let schema = parse_struct_to_schema(&struct_item, &HashMap::new(), &HashMap::new());
        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }

    #[test]
    fn test_parse_type_to_schema_ref_empty_path_and_reference() {
        // Empty path segments returns object
        let ty = Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let schema_ref = parse_type_to_schema_ref(&ty, &HashMap::new(), &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));

        // Reference type delegates to inner
        let ty: Type = syn::parse_str("&i32").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashMap::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline integer schema");
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_known_schema_ref_and_unknown_custom() {
        let mut known_schemas = HashMap::new();
        known_schemas.insert("Known".to_string(), "Known".to_string());

        let ty: Type = syn::parse_str("Known").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Ref(_)));

        let ty: Type = syn::parse_str("UnknownType").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));
    }

    #[test]
    fn test_parse_type_to_schema_ref_generic_substitution() {
        // Ensure generic struct Wrapper<T> { value: T } is substituted to concrete type
        let mut known_schemas = HashMap::new();
        known_schemas.insert("Wrapper".to_string(), "Wrapper".to_string());

        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "Wrapper".to_string(),
            "struct Wrapper<T> { value: T }".to_string(),
        );

        let ty: syn::Type = syn::parse_str("Wrapper<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &struct_definitions);

        if let SchemaRef::Inline(schema) = schema_ref {
            let props = schema.properties.as_ref().unwrap();
            let value = props.get("value").unwrap();
            if let SchemaRef::Inline(inner) = value {
                assert_eq!(inner.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline schema for value");
            }
        } else {
            panic!("Expected inline schema for generic substitution");
        }
    }

    #[rstest]
    #[case("$invalid", "String")]
    fn test_substitute_type_parse_failure_uses_original(
        #[case] invalid: &str,
        #[case] concrete_src: &str,
    ) {
        use proc_macro2::TokenStream;
        use std::str::FromStr;

        let ty = Type::Verbatim(TokenStream::from_str(invalid).unwrap());
        let concrete: Type = syn::parse_str(concrete_src).unwrap();
        let substituted = substitute_type(&ty, &[String::from("T")], &[&concrete]);
        assert_eq!(substituted, ty);
    }

    #[rstest]
    #[case("&i32")]
    #[case("std::string::String")]
    fn test_is_primitive_type_non_path_variants(#[case] ty_src: &str) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        assert!(!is_primitive_type(&ty));
    }

    #[rstest]
    #[case(
        "HashMap<String, Value>",
        true,
        None,
        Some("#/components/schemas/Value")
    )]
    #[case("Result<String, i32>", false, Some(SchemaType::Object), None)]
    #[case("crate::Value", false, None, None)]
    #[case("(i32, bool)", false, Some(SchemaType::Object), None)]
    fn test_parse_type_to_schema_ref_additional_cases(
        #[case] ty_src: &str,
        #[case] expect_additional_props: bool,
        #[case] expected_type: Option<SchemaType>,
        #[case] expected_ref: Option<&str>,
    ) {
        let mut known_schemas = HashMap::new();
        known_schemas.insert("Value".to_string(), "Value".to_string());

        let ty: Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        match expected_ref {
            Some(expected) => {
                let SchemaRef::Inline(schema) = schema_ref else {
                    panic!("Expected inline schema for {}", ty_src);
                };
                let additional = schema
                    .additional_properties
                    .as_ref()
                    .expect("additional_properties missing");
                assert_eq!(additional.get("$ref").unwrap(), expected);
            }
            None => match schema_ref {
                SchemaRef::Inline(schema) => {
                    if expect_additional_props {
                        assert!(schema.additional_properties.is_some());
                    } else {
                        assert_eq!(schema.schema_type, expected_type);
                    }
                }
                SchemaRef::Ref(_) => {
                    assert!(ty_src.contains("Value"));
                }
            },
        }
    }

    #[rstest]
    // camelCase tests
    #[case("user_name", Some("camelCase"), "userName")]
    #[case("first_name", Some("camelCase"), "firstName")]
    #[case("last_name", Some("camelCase"), "lastName")]
    #[case("user_id", Some("camelCase"), "userId")]
    #[case("api_key", Some("camelCase"), "apiKey")]
    #[case("already_camel", Some("camelCase"), "alreadyCamel")]
    // snake_case tests
    #[case("userName", Some("snake_case"), "user_name")]
    #[case("firstName", Some("snake_case"), "first_name")]
    #[case("lastName", Some("snake_case"), "last_name")]
    #[case("userId", Some("snake_case"), "user_id")]
    #[case("apiKey", Some("snake_case"), "api_key")]
    #[case("already_snake", Some("snake_case"), "already_snake")]
    // kebab-case tests
    #[case("user_name", Some("kebab-case"), "user-name")]
    #[case("first_name", Some("kebab-case"), "first-name")]
    #[case("last_name", Some("kebab-case"), "last-name")]
    #[case("user_id", Some("kebab-case"), "user-id")]
    #[case("api_key", Some("kebab-case"), "api-key")]
    #[case("already-kebab", Some("kebab-case"), "already-kebab")]
    // PascalCase tests
    #[case("user_name", Some("PascalCase"), "UserName")]
    #[case("first_name", Some("PascalCase"), "FirstName")]
    #[case("last_name", Some("PascalCase"), "LastName")]
    #[case("user_id", Some("PascalCase"), "UserId")]
    #[case("api_key", Some("PascalCase"), "ApiKey")]
    #[case("AlreadyPascal", Some("PascalCase"), "AlreadyPascal")]
    // lowercase tests
    #[case("UserName", Some("lowercase"), "username")]
    #[case("FIRST_NAME", Some("lowercase"), "first_name")]
    #[case("lastName", Some("lowercase"), "lastname")]
    #[case("User_ID", Some("lowercase"), "user_id")]
    #[case("API_KEY", Some("lowercase"), "api_key")]
    #[case("already_lower", Some("lowercase"), "already_lower")]
    // UPPERCASE tests
    #[case("user_name", Some("UPPERCASE"), "USER_NAME")]
    #[case("firstName", Some("UPPERCASE"), "FIRSTNAME")]
    #[case("LastName", Some("UPPERCASE"), "LASTNAME")]
    #[case("user_id", Some("UPPERCASE"), "USER_ID")]
    #[case("apiKey", Some("UPPERCASE"), "APIKEY")]
    #[case("ALREADY_UPPER", Some("UPPERCASE"), "ALREADY_UPPER")]
    // SCREAMING_SNAKE_CASE tests
    #[case("user_name", Some("SCREAMING_SNAKE_CASE"), "USER_NAME")]
    #[case("firstName", Some("SCREAMING_SNAKE_CASE"), "FIRST_NAME")]
    #[case("LastName", Some("SCREAMING_SNAKE_CASE"), "LAST_NAME")]
    #[case("user_id", Some("SCREAMING_SNAKE_CASE"), "USER_ID")]
    #[case("apiKey", Some("SCREAMING_SNAKE_CASE"), "API_KEY")]
    #[case("ALREADY_SCREAMING", Some("SCREAMING_SNAKE_CASE"), "ALREADY_SCREAMING")]
    // SCREAMING-KEBAB-CASE tests
    #[case("user_name", Some("SCREAMING-KEBAB-CASE"), "USER-NAME")]
    #[case("firstName", Some("SCREAMING-KEBAB-CASE"), "FIRST-NAME")]
    #[case("LastName", Some("SCREAMING-KEBAB-CASE"), "LAST-NAME")]
    #[case("user_id", Some("SCREAMING-KEBAB-CASE"), "USER-ID")]
    #[case("apiKey", Some("SCREAMING-KEBAB-CASE"), "API-KEY")]
    #[case("already-kebab", Some("SCREAMING-KEBAB-CASE"), "ALREADY-KEBAB")]
    // None tests (no transformation)
    #[case("user_name", None, "user_name")]
    #[case("firstName", None, "firstName")]
    #[case("LastName", None, "LastName")]
    #[case("user-id", None, "user-id")]
    fn test_rename_field(
        #[case] field_name: &str,
        #[case] rename_all: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(rename_field(field_name, rename_all), expected);
    }
}
