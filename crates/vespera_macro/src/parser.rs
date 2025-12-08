//! Parser module for analyzing function signatures and converting to OpenAPI structures

use std::collections::{BTreeMap, HashMap};
use syn::{Fields, FnArg, Pat, PatType, ReturnType, Type};
use vespera_core::{
    route::{Header, MediaType, Operation, Parameter, ParameterLocation, RequestBody, Response},
    schema::{Reference, Schema, SchemaRef, SchemaType},
};

/// Extract path parameters from a path string
pub fn extract_path_parameters(path: &str) -> Vec<String> {
    let mut params = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();

    for segment in segments {
        if segment.starts_with('{') && segment.ends_with('}') {
            let param = segment.trim_start_matches('{').trim_end_matches('}');
            params.push(param.to_string());
        } else if segment.starts_with(':') {
            let param = segment.trim_start_matches(':');
            params.push(param.to_string());
        }
    }

    params
}

/// Analyze function parameter and convert to OpenAPI Parameter(s)
/// Returns None if parameter should be ignored (e.g., Query<HashMap<...>>)
/// Returns Some(Vec<Parameter>) with one or more parameters
pub fn parse_function_parameter(
    arg: &FnArg,
    path_params: &[String],
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { pat, ty, .. }) => {
            // Extract parameter name from pattern
            let param_name = match pat.as_ref() {
                Pat::Ident(ident) => ident.ident.to_string(),
                Pat::TupleStruct(tuple_struct) => {
                    // Handle Path(id) pattern
                    if tuple_struct.elems.len() == 1 {
                        match &tuple_struct.elems[0] {
                            Pat::Ident(ident) => ident.ident.to_string(),
                            _ => return None,
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            };

            // Check for common Axum extractors first (before checking path_params)
            // Handle both Path<T> and vespera::axum::extract::Path<T> by checking the last segment
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if !path.segments.is_empty() {
                    // Check the last segment (handles both Path<T> and vespera::axum::extract::Path<T>)
                    let segment = path.segments.last().unwrap();
                    let ident_str = segment.ident.to_string();

                    match ident_str.as_str() {
                        "Path" => {
                            // Path<T> extractor - use path parameter name from route if available
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                // Check if inner type is a tuple (e.g., Path<(String, String, String)>)
                                if let Type::Tuple(tuple) = inner_ty {
                                    // For tuple types, extract parameters from path string
                                    let mut parameters = Vec::new();
                                    let tuple_elems = &tuple.elems;

                                    // Match tuple elements with path parameters
                                    for (idx, elem_ty) in tuple_elems.iter().enumerate() {
                                        if let Some(param_name) = path_params.get(idx) {
                                            parameters.push(Parameter {
                                                name: param_name.clone(),
                                                r#in: ParameterLocation::Path,
                                                description: None,
                                                required: Some(true),
                                                schema: Some(
                                                    parse_type_to_schema_ref_with_schemas(
                                                        elem_ty,
                                                        known_schemas,
                                                        struct_definitions,
                                                    ),
                                                ),
                                                example: None,
                                            });
                                        }
                                    }

                                    if !parameters.is_empty() {
                                        return Some(parameters);
                                    }
                                } else {
                                    // Single path parameter
                                    // If there's exactly one path parameter, use its name
                                    let name = if path_params.len() == 1 {
                                        path_params[0].clone()
                                    } else {
                                        // Otherwise use the parameter name from the pattern
                                        param_name
                                    };
                                    return Some(vec![Parameter {
                                        name,
                                        r#in: ParameterLocation::Path,
                                        description: None,
                                        required: Some(true),
                                        schema: Some(parse_type_to_schema_ref_with_schemas(
                                            inner_ty,
                                            known_schemas,
                                            struct_definitions,
                                        )),
                                        example: None,
                                    }]);
                                }
                            }
                        }
                        "Query" => {
                            // Query<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                // Check if it's HashMap or BTreeMap - ignore these
                                if is_map_type(inner_ty) {
                                    return None;
                                }

                                // Check if it's a struct - expand to individual parameters
                                if let Some(struct_params) = parse_query_struct_to_parameters(
                                    inner_ty,
                                    known_schemas,
                                    struct_definitions,
                                ) {
                                    return Some(struct_params);
                                }

                                // Check if it's a known type (primitive or known schema)
                                // If unknown, don't add parameter
                                if !is_known_type(inner_ty, known_schemas, struct_definitions) {
                                    return None;
                                }

                                // Otherwise, treat as single parameter
                                return Some(vec![Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Query,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref_with_schemas(
                                        inner_ty,
                                        known_schemas,
                                        struct_definitions,
                                    )),
                                    example: None,
                                }]);
                            }
                        }
                        "Header" => {
                            // Header<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                return Some(vec![Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Header,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref_with_schemas(
                                        inner_ty,
                                        known_schemas,
                                        struct_definitions,
                                    )),
                                    example: None,
                                }]);
                            }
                        }
                        "Json" => {
                            // Json<T> extractor - this will be handled as RequestBody
                            return None;
                        }
                        _ => {}
                    }
                }
            }

            // Check if it's a path parameter (by name match) - for non-extractor cases
            if path_params.contains(&param_name) {
                return Some(vec![Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref_with_schemas(
                        ty,
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                }]);
            }

            // Check if it's a primitive type (direct parameter)
            if is_primitive_type(ty.as_ref()) {
                return Some(vec![Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Query,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref_with_schemas(
                        ty,
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                }]);
            }

            None
        }
    }
}

/// Check if a type is HashMap or BTreeMap
fn is_map_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            let segment = path.segments.last().unwrap();
            let ident_str = segment.ident.to_string();
            return ident_str == "HashMap" || ident_str == "BTreeMap";
        }
    }
    false
}

/// Check if a type is a known type (primitive, known schema, or struct definition)
fn is_known_type(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> bool {
    // Check if it's a primitive type
    if is_primitive_type(ty) {
        return true;
    }

    // Check if it's a known struct
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return false;
        }

        let segment = path.segments.last().unwrap();
        let ident_str = segment.ident.to_string();

        // Get type name (handle both simple and qualified paths)

        // Check if it's in struct_definitions or known_schemas
        if struct_definitions.contains_key(&ident_str) || known_schemas.contains_key(&ident_str) {
            return true;
        }

        // Check for generic types like Vec<T>, Option<T> - recursively check inner type
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
            match ident_str.as_str() {
                "Vec" | "Option" => {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return is_known_type(inner_ty, known_schemas, struct_definitions);
                    }
                }
                _ => {}
            }
        }
    }

    false
}

/// Parse struct fields to individual query parameters
/// Returns None if the type is not a struct or cannot be parsed
fn parse_query_struct_to_parameters(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    // Check if it's a known struct
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return None;
        }

        let segment = path.segments.last().unwrap();
        let ident_str = segment.ident.to_string();

        // Get type name (handle both simple and qualified paths)

        // Check if it's a known struct
        if let Some(struct_def) = struct_definitions.get(&ident_str)
            && let Ok(struct_item) = syn::parse_str::<syn::ItemStruct>(struct_def)
        {
            let mut parameters = Vec::new();

            // Extract rename_all attribute from struct
            let rename_all = extract_rename_all(&struct_item.attrs);

            if let syn::Fields::Named(fields_named) = &struct_item.fields {
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
                        rename_field(&rust_field_name, rename_all.as_deref())
                    };

                    let field_type = &field.ty;

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

                    // Parse field type to schema (inline, not ref)
                    // For Query parameters, we need inline schemas, not refs
                    let mut field_schema = parse_type_to_schema_ref_with_schemas(
                        field_type,
                        known_schemas,
                        struct_definitions,
                    );

                    // Convert ref to inline if needed (Query parameters should not use refs)
                    // If it's a ref to a known struct, get the struct definition and inline it
                    if let SchemaRef::Ref(ref_ref) = &field_schema {
                        // Try to extract type name from ref path (e.g., "#/components/schemas/User" -> "User")
                        if let Some(type_name) =
                            ref_ref.ref_path.strip_prefix("#/components/schemas/")
                            && let Some(struct_def) = struct_definitions.get(type_name)
                            && let Ok(nested_struct_item) =
                                syn::parse_str::<syn::ItemStruct>(struct_def)
                        {
                            // Parse the nested struct to schema (inline)
                            let nested_schema = parse_struct_to_schema(
                                &nested_struct_item,
                                known_schemas,
                                struct_definitions,
                            );
                            field_schema = SchemaRef::Inline(Box::new(nested_schema));
                        }
                    }

                    // If it's Option<T>, make it nullable
                    let final_schema = if is_optional {
                        if let SchemaRef::Inline(mut schema) = field_schema {
                            schema.nullable = Some(true);
                            SchemaRef::Inline(schema)
                        } else {
                            // If still a ref, convert to inline object with nullable
                            SchemaRef::Inline(Box::new(Schema {
                                schema_type: Some(SchemaType::Object),
                                nullable: Some(true),
                                ..Schema::object()
                            }))
                        }
                    } else {
                        // If it's still a ref, convert to inline object
                        match field_schema {
                            SchemaRef::Ref(_) => {
                                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                            }
                            SchemaRef::Inline(schema) => SchemaRef::Inline(schema),
                        }
                    };

                    let required = !is_optional;

                    parameters.push(Parameter {
                        name: field_name,
                        r#in: ParameterLocation::Query,
                        description: None,
                        required: Some(required),
                        schema: Some(final_schema),
                        example: None,
                    });
                }
            }

            if !parameters.is_empty() {
                return Some(parameters);
            }
        }
    }
    None
}

/// Check if a type is a primitive type
fn is_primitive_type(ty: &Type) -> bool {
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

/// Extract rename_all attribute from struct attributes
fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
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

/// Extract rename attribute from field attributes
/// Handles #[serde(rename = "newName")]
fn extract_field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try to parse as Meta::List first
            if let syn::Meta::List(meta_list) = &attr.meta {
                let tokens = meta_list.tokens.to_string();

                // Look for rename = "..." pattern
                if let Some(start) = tokens.find("rename") {
                    let remaining = &tokens[start + "rename".len()..];
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
    }
    None
}

/// Convert field name according to rename_all rule
fn rename_field(field_name: &str, rename_all: Option<&str>) -> String {
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
            // Convert snake_case to kebab-case
            field_name.replace('_', "-")
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

/// Parse enum definition to OpenAPI Schema
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
                        schema_type: Some(SchemaType::String),
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
                            schema_type: Some(SchemaType::Object),
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
                            schema_type: Some(SchemaType::Array),
                            prefix_items: Some(tuple_item_schemas),
                            min_items: Some(tuple_len),
                            max_items: Some(tuple_len),
                            items: None, // prefixItems와 items는 함께 사용하지 않음
                            ..Schema::new(SchemaType::Array)
                        };

                        let mut properties = BTreeMap::new();
                        properties.insert(
                            variant_key.clone(),
                            SchemaRef::Inline(Box::new(array_schema)),
                        );

                        Schema {
                            schema_type: Some(SchemaType::Object),
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
                        schema_type: Some(SchemaType::Object),
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
                        schema_type: Some(SchemaType::Object),
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

/// Parse struct definition to OpenAPI Schema
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

                let schema_ref =
                    parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                properties.insert(field_name.clone(), schema_ref);

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
                    required.push(field_name);
                }
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

/// Substitute generic parameters in a type with concrete types
/// Uses quote! to regenerate the type with substitutions
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

/// Parse Rust type to OpenAPI SchemaRef
pub fn parse_type_to_schema_ref(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    parse_type_to_schema_ref_with_schemas(ty, known_schemas, struct_definitions)
}

/// Parse Rust type to OpenAPI SchemaRef with optional schemas map for resolving references
fn parse_type_to_schema_ref_with_schemas(
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
                                if let SchemaRef::Inline(mut schema) = inner_schema {
                                    schema.nullable = Some(true);
                                    return SchemaRef::Inline(schema);
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

/// Analyze function signature and extract RequestBody
pub fn parse_request_body(
    arg: &FnArg,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<RequestBody> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { ty, .. }) => {
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if path.segments.is_empty() {
                    return None;
                }

                // Check the last segment (handles both Json<T> and vespera::axum::Json<T>)
                let segment = path.segments.last().unwrap();
                let ident_str = segment.ident.to_string();

                if ident_str == "Json"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                {
                    let schema = parse_type_to_schema_ref_with_schemas(
                        inner_ty,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut content = BTreeMap::new();
                    content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(schema),
                            example: None,
                            examples: None,
                        },
                    );
                    return Some(RequestBody {
                        description: None,
                        required: Some(true),
                        content,
                    });
                }
            }
            None
        }
    }
}

/// Unwrap Json<T> to get T
/// Handles both Json<T> and vespera::axum::Json<T> by checking the last segment
fn unwrap_json(ty: &Type) -> &Type {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            // Check the last segment (handles both Json<T> and vespera::axum::Json<T>)
            let segment = path.segments.last().unwrap();
            if segment.ident == "Json"
                && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
            {
                return inner_ty;
            }
        }
    }
    ty
}

/// Extract Ok and Err types from Result<T, E> or Result<Json<T>, E>
/// Handles both Result and std::result::Result, and unwraps references
fn extract_result_types(ty: &Type) -> Option<(Type, Type)> {
    // First unwrap Json if present
    let unwrapped = unwrap_json(ty);

    // Handle both Type::Path and Type::Reference (for &Result<...>)
    let result_type = match unwrapped {
        Type::Path(type_path) => type_path,
        Type::Reference(type_ref) => {
            // Unwrap reference and check if it's a Result
            if let Type::Path(type_path) = type_ref.elem.as_ref() {
                type_path
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let path = &result_type.path;
    if path.segments.is_empty() {
        return None;
    }

    // Check if any segment is "Result" (handles both Result and std::result::Result)
    let is_result = path.segments.iter().any(|seg| seg.ident == "Result");

    if is_result {
        // Get the last segment (Result) to check for generics
        if let Some(segment) = path.segments.last()
            && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && args.args.len() >= 2
            && let (
                Some(syn::GenericArgument::Type(ok_ty)),
                Some(syn::GenericArgument::Type(err_ty)),
            ) = (args.args.first(), args.args.get(1))
        {
            // Unwrap Json from Ok type if present
            let ok_ty_unwrapped = unwrap_json(ok_ty);
            return Some((ok_ty_unwrapped.clone(), err_ty.clone()));
        }
    }
    None
}

/// Check if error type is a tuple (StatusCode, E) or (StatusCode, Json<E>)
/// Returns the error type E and a default status code (400)
fn extract_status_code_tuple(err_ty: &Type) -> Option<(u16, Type)> {
    if let Type::Tuple(tuple) = err_ty
        && tuple.elems.len() == 2
    {
        // Check if first element is StatusCode
        if let Type::Path(type_path) = &tuple.elems[0] {
            let path = &type_path.path;
            if !path.segments.is_empty() {
                let segment = &path.segments[0];
                // Check if it's StatusCode (could be qualified like axum::http::StatusCode)
                let is_status_code = segment.ident == "StatusCode"
                    || (path.segments.len() > 1
                        && path.segments.iter().any(|s| s.ident == "StatusCode"));

                if is_status_code {
                    // Use 400 as default status code
                    // The actual status code value is determined at runtime
                    if let Some(error_type) = tuple.elems.get(1) {
                        // Unwrap Json if present
                        let error_type_unwrapped = unwrap_json(error_type);
                        return Some((400, error_type_unwrapped.clone()));
                    }
                }
            }
        }
    }
    None
}

/// Check whether the provided type is a HeaderMap
fn is_header_map_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return false;
        }
        return path.segments.iter().any(|s| s.ident == "HeaderMap");
    }
    false
}

/// Extract payload type from an Ok tuple and track if headers exist.
/// The last element of the tuple is always treated as the response body.
/// Any presence of HeaderMap in the tuple marks headers as present.
fn extract_ok_payload_and_headers(ok_ty: &Type) -> (Type, Option<HashMap<String, Header>>) {
    if let Type::Tuple(tuple) = ok_ty {
        let payload_ty = tuple.elems.last().map(|ty| unwrap_json(ty).clone());
        let has_headers = tuple.elems.iter().any(is_header_map_type);

        if let Some(payload_ty) = payload_ty {
            let headers = if has_headers {
                Some(HashMap::new())
            } else {
                None
            };
            return (payload_ty, headers);
        }
    }

    (ok_ty.clone(), None)
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> BTreeMap<String, Response> {
    let mut responses = BTreeMap::new();

    match return_type {
        ReturnType::Default => {
            // No return type - just 200 with no content
            responses.insert(
                "200".to_string(),
                Response {
                    description: "Successful response".to_string(),
                    headers: None,
                    content: None,
                },
            );
        }
        ReturnType::Type(_, ty) => {
            // Check if it's a Result<T, E>
            if let Some((ok_ty, err_ty)) = extract_result_types(ty) {
                // Handle success response (200)
                let (ok_payload_ty, ok_headers) = extract_ok_payload_and_headers(&ok_ty);
                let ok_schema = parse_type_to_schema_ref_with_schemas(
                    &ok_payload_ty,
                    known_schemas,
                    struct_definitions,
                );
                let mut ok_content = BTreeMap::new();
                ok_content.insert(
                    "application/json".to_string(),
                    MediaType {
                        schema: Some(ok_schema),
                        example: None,
                        examples: None,
                    },
                );

                responses.insert(
                    "200".to_string(),
                    Response {
                        description: "Successful response".to_string(),
                        headers: ok_headers,
                        content: Some(ok_content),
                    },
                );

                // Handle error response
                // Check if error is (StatusCode, E) tuple
                if let Some((status_code, error_type)) = extract_status_code_tuple(&err_ty) {
                    // Use the status code from the tuple
                    let err_schema = parse_type_to_schema_ref_with_schemas(
                        &error_type,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(err_schema),
                            example: None,
                            examples: None,
                        },
                    );

                    responses.insert(
                        status_code.to_string(),
                        Response {
                            description: "Error response".to_string(),
                            headers: None,
                            content: Some(err_content),
                        },
                    );
                } else {
                    // Regular error type - use default 400
                    // Unwrap Json if present
                    let err_ty_unwrapped = unwrap_json(&err_ty);
                    let err_schema = parse_type_to_schema_ref_with_schemas(
                        err_ty_unwrapped,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(err_schema),
                            example: None,
                            examples: None,
                        },
                    );

                    responses.insert(
                        "400".to_string(),
                        Response {
                            description: "Error response".to_string(),
                            headers: None,
                            content: Some(err_content),
                        },
                    );
                }
            } else {
                // Not a Result type - regular response
                // Unwrap Json<T> if present
                let unwrapped_ty = unwrap_json(ty);
                let schema = parse_type_to_schema_ref_with_schemas(
                    unwrapped_ty,
                    known_schemas,
                    struct_definitions,
                );
                let mut content = BTreeMap::new();
                content.insert(
                    "application/json".to_string(),
                    MediaType {
                        schema: Some(schema),
                        example: None,
                        examples: None,
                    },
                );

                responses.insert(
                    "200".to_string(),
                    Response {
                        description: "Successful response".to_string(),
                        headers: None,
                        content: Some(content),
                    },
                );
            }
        }
    }

    responses
}

/// Build Operation from function signature
pub fn build_operation_from_function(
    sig: &syn::Signature,
    path: &str,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
    error_status: Option<&[u16]>,
) -> Operation {
    let path_params = extract_path_parameters(path);
    let mut parameters = Vec::new();
    let mut request_body = None;
    let mut path_extractor_type: Option<Type> = None;

    // First pass: find Path<T> extractor and extract its type
    for input in &sig.inputs {
        if let FnArg::Typed(PatType { ty, .. }) = input
            && let Type::Path(type_path) = ty.as_ref()
        {
            let path_segments = &type_path.path;
            if !path_segments.segments.is_empty() {
                let segment = path_segments.segments.last().unwrap();
                if segment.ident == "Path"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                {
                    path_extractor_type = Some(inner_ty.clone());
                    break;
                }
            }
        }
    }

    // Generate path parameters from path string (not from function signature)
    // This is the primary source of truth for path parameters
    if !path_params.is_empty() {
        if let Some(ty) = path_extractor_type {
            // Check if it's a tuple type
            if let Type::Tuple(tuple) = ty {
                // For tuple types, match each path parameter with tuple element type
                for (idx, param_name) in path_params.iter().enumerate() {
                    if let Some(elem_ty) = tuple.elems.get(idx) {
                        parameters.push(Parameter {
                            name: param_name.clone(),
                            r#in: ParameterLocation::Path,
                            description: None,
                            required: Some(true),
                            schema: Some(parse_type_to_schema_ref_with_schemas(
                                elem_ty,
                                known_schemas,
                                struct_definitions,
                            )),
                            example: None,
                        });
                    } else {
                        // If tuple doesn't have enough elements, use String as default
                        parameters.push(Parameter {
                            name: param_name.clone(),
                            r#in: ParameterLocation::Path,
                            description: None,
                            required: Some(true),
                            schema: Some(parse_type_to_schema_ref_with_schemas(
                                &syn::parse_str::<Type>("String").unwrap(),
                                known_schemas,
                                struct_definitions,
                            )),
                            example: None,
                        });
                    }
                }
            } else {
                // Single path parameter
                if path_params.len() == 1 {
                    parameters.push(Parameter {
                        name: path_params[0].clone(),
                        r#in: ParameterLocation::Path,
                        description: None,
                        required: Some(true),
                        schema: Some(parse_type_to_schema_ref_with_schemas(
                            &ty,
                            known_schemas,
                            struct_definitions,
                        )),
                        example: None,
                    });
                } else {
                    // Multiple path parameters but single type - use String for all
                    for param_name in &path_params {
                        parameters.push(Parameter {
                            name: param_name.clone(),
                            r#in: ParameterLocation::Path,
                            description: None,
                            required: Some(true),
                            schema: Some(parse_type_to_schema_ref_with_schemas(
                                &ty,
                                known_schemas,
                                struct_definitions,
                            )),
                            example: None,
                        });
                    }
                }
            }
        } else {
            // No Path extractor found, but path has parameters - use String as default
            for param_name in &path_params {
                parameters.push(Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref_with_schemas(
                        &syn::parse_str::<Type>("String").unwrap(),
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                });
            }
        }
    }

    // Parse function parameters (skip Path extractor as we already handled it)
    for input in &sig.inputs {
        // Check if it's a request body (Json<T>)
        if let Some(body) = parse_request_body(input, known_schemas, struct_definitions) {
            request_body = Some(body);
        } else {
            // Skip Path extractor - we already handled path parameters above
            let is_path_extractor = if let FnArg::Typed(PatType { ty, .. }) = input {
                if let Type::Path(type_path) = ty.as_ref() {
                    let path_segments = &type_path.path;
                    if !path_segments.segments.is_empty() {
                        let segment = path_segments.segments.last().unwrap();
                        segment.ident == "Path"
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if !is_path_extractor {
                // Process non-Path parameters
                if let Some(params) =
                    parse_function_parameter(input, &path_params, known_schemas, struct_definitions)
                {
                    parameters.extend(params);
                }
            }
        }
    }

    // Parse return type - may return multiple responses (for Result types)
    let mut responses = parse_return_type(&sig.output, known_schemas, struct_definitions);

    // Add additional error status codes from error_status attribute
    if let Some(status_codes) = error_status {
        // Find the error response schema (usually 400 or the first error response)
        let error_schema = responses
            .iter()
            .find(|(code, _)| code != &&"200".to_string())
            .and_then(|(_, resp)| {
                resp.content
                    .as_ref()?
                    .get("application/json")?
                    .schema
                    .clone()
            });

        if let Some(schema) = error_schema {
            for &status_code in status_codes {
                let status_str = status_code.to_string();
                // Only add if not already present
                responses.entry(status_str).or_insert_with(|| {
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(schema.clone()),
                            example: None,
                            examples: None,
                        },
                    );

                    Response {
                        description: "Error response".to_string(),
                        headers: None,
                        content: Some(err_content),
                    }
                });
            }
        }
    }

    Operation {
        operation_id: Some(sig.ident.to_string()),
        tags: None,
        summary: None,
        description: None,
        parameters: if parameters.is_empty() {
            None
        } else {
            Some(parameters)
        },
        request_body,
        responses,
        security: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;
    use std::collections::HashMap;
    use vespera_core::schema::SchemaType;

    #[rstest]
    #[case("/test", vec![])]
    #[case("/test/{id}", vec!["id"])]
    #[case("/test/{id}/test/{test_id}", vec!["id", "test_id"])]
    #[case("/test/:id/test/:test_id", vec!["id", "test_id"])]
    fn test_extract_path_parameters(#[case] path: &str, #[case] expected: Vec<&str>) {
        assert_eq!(extract_path_parameters(path), expected);
    }

    #[rstest]
    #[case("", "")] // No return type
    #[case("-> String", "String")] // Simple return type
    #[case("-> i32", "i32")] // Integer return type
    #[case("-> bool", "bool")] // Boolean return type
    #[case("-> Vec<String>", "Vec<String>")] // Array return type
    #[case("-> Option<String>", "Option<String>")] // Option return type
    #[case("-> Result<String, String>", "Result<String, String>")] // Result with same types
    #[case("-> Result<i32, String>", "Result<i32, String>")] // Result with different types
    #[case("-> Result<Json<User>, String>", "Result<Json<User>, String>")] // Result with Json wrapper
    #[case(
        "-> Result<String, (StatusCode, String)>",
        "Result<String, (StatusCode, String)>"
    )] // Result with status code tuple
    #[case("-> &str", "&str")] // Reference return type
    #[case("-> Result<&str, String>", "Result<&str, String>")] // Result with reference
    fn test_parse_return_type(#[case] return_type_str: &str, #[case] expected_type: &str) {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let return_type = if return_type_str.is_empty() {
            ReturnType::Default
        } else {
            // Parse the return type from string
            let full_signature = format!("fn test() {}", return_type_str);
            let parsed: syn::Signature =
                syn::parse_str(&full_signature).expect("Failed to parse return type");
            parsed.output
        };

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        match expected_type {
            "" => {
                // ReturnType::Default - should have 200 with no content
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_none());
            }
            "String" | "&str" => {
                // String return type - should have 200 with String schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema");
                }
            }
            "i32" => {
                // Integer return type - should have 200 with Integer schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
                } else {
                    panic!("Expected inline Integer schema");
                }
            }
            "bool" => {
                // Boolean return type - should have 200 with Boolean schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Boolean));
                } else {
                    panic!("Expected inline Boolean schema");
                }
            }
            "Vec<String>" => {
                // Array return type - should have 200 with Array schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Array));
                    assert!(schema.items.is_some());
                    // Check that items is String
                    if let Some(items) = &schema.items {
                        if let SchemaRef::Inline(items_schema) = items.as_ref() {
                            assert_eq!(items_schema.schema_type, Some(SchemaType::String));
                        }
                    }
                } else {
                    panic!("Expected inline Array schema");
                }
            }
            "Option<String>" => {
                // Option return type - should have 200 with nullable String schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.nullable, Some(true));
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline nullable String schema");
                }
            }
            "Result<String, String>" => {
                // Result types - should have 200 for Ok and 400 for Err
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<i32, String>" => {
                // Result types - should have 200 for Ok and 400 for Err
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
                } else {
                    panic!("Expected inline Integer schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<Json<User>, String>" => {
                // Result with Json wrapper - should unwrap Json
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                // User is not in known_schemas, so it should be an object schema
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Object));
                } else {
                    panic!("Expected inline Object schema for User type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<&str, String>" => {
                // Result with reference - should handle reference correctly
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for &str type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<String, (StatusCode, String)>" => {
                // Result with status code tuple - should use status code from tuple
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400")); // Default status code from tuple

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            _ => panic!("Unexpected test case"),
        }
    }

    #[test]
    fn test_parse_return_type_with_known_schema() {
        let mut known_schemas = HashMap::new();
        known_schemas.insert("User".to_string(), "User".to_string());
        let struct_definitions = HashMap::new();
        {
            let return_type_str = "-> User";
            let full_signature = format!("fn test() {}", return_type_str);
            let parsed: syn::Signature =
                syn::parse_str(&full_signature).expect("Failed to parse return type");

            let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

            assert_eq!(responses.len(), 1);
            assert!(responses.contains_key("200"));
            let response = responses.get("200").unwrap();
            assert!(response.content.is_some());

            let content = response.content.as_ref().unwrap();
            let media_type = content.get("application/json").unwrap();

            // Should be a reference to the known schema
            if let SchemaRef::Ref(ref_ref) = media_type.schema.as_ref().unwrap() {
                assert_eq!(ref_ref.ref_path, "#/components/schemas/User");
            } else {
                panic!("Expected schema reference for known type");
            }
        }
        {
            let return_type_str = "-> Json<User>";
            let full_signature = format!("fn test() {}", return_type_str);
            let parsed: syn::Signature =
                syn::parse_str(&full_signature).expect("Failed to parse return type");

            println!("parsed: {:?}", parsed.output);
            let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);
            println!("responses: {:?}", responses);

            assert_eq!(responses.len(), 1);
            assert!(responses.contains_key("200"));
            let response = responses.get("200").unwrap();
            assert!(response.content.is_some());

            let content = response.content.as_ref().unwrap();
            let media_type = content.get("application/json").unwrap();

            // Should be a reference to the known schema
            if let SchemaRef::Ref(ref_ref) = media_type.schema.as_ref().unwrap() {
                assert_eq!(ref_ref.ref_path, "#/components/schemas/User");
            } else {
                panic!("Expected schema reference for Json<User>");
            }
        }
    }

    #[test]
    fn test_parse_return_type_result_with_known_schema() {
        let mut known_schemas = HashMap::new();
        known_schemas.insert("User".to_string(), "User".to_string());
        known_schemas.insert("Error".to_string(), "Error".to_string());
        let struct_definitions = HashMap::new();

        let return_type_str = "-> Result<User, Error>";
        let full_signature = format!("fn test() {}", return_type_str);
        let parsed: syn::Signature =
            syn::parse_str(&full_signature).expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        assert_eq!(responses.len(), 2);
        assert!(responses.contains_key("200"));
        assert!(responses.contains_key("400"));

        // Check Ok response has User schema reference
        let ok_response = responses.get("200").unwrap();
        let ok_content = ok_response.content.as_ref().unwrap();
        let ok_media_type = ok_content.get("application/json").unwrap();
        if let SchemaRef::Ref(ref_ref) = ok_media_type.schema.as_ref().unwrap() {
            assert_eq!(ref_ref.ref_path, "#/components/schemas/User");
        } else {
            panic!("Expected schema reference for User");
        }

        // Check Err response has Error schema reference
        let err_response = responses.get("400").unwrap();
        let err_content = err_response.content.as_ref().unwrap();
        let err_media_type = err_content.get("application/json").unwrap();
        if let SchemaRef::Ref(ref_ref) = err_media_type.schema.as_ref().unwrap() {
            assert_eq!(ref_ref.ref_path, "#/components/schemas/Error");
        } else {
            panic!("Expected schema reference for Error");
        }
    }

    #[test]
    fn test_parse_return_type_with_header_map_tuple() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let parsed: syn::Signature =
            syn::parse_str("fn test() -> Result<(HeaderMap, String), String>")
                .expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        let ok_response = responses.get("200").expect("Ok response missing");
        let ok_content = ok_response
            .content
            .as_ref()
            .expect("Ok content missing")
            .get("application/json")
            .expect("application/json missing");

        if let SchemaRef::Inline(schema) = ok_content.schema.as_ref().unwrap() {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
        } else {
            panic!("Expected inline String schema for Ok type");
        }

        assert!(
            ok_response.headers.is_some(),
            "HeaderMap should set headers"
        );
    }

    #[test]
    fn test_parse_return_type_with_status_and_header_map_tuple() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let parsed: syn::Signature =
            syn::parse_str("fn test() -> Result<(StatusCode, HeaderMap, String), String>")
                .expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        let ok_response = responses.get("200").expect("Ok response missing");
        let ok_content = ok_response
            .content
            .as_ref()
            .expect("Ok content missing")
            .get("application/json")
            .expect("application/json missing");

        if let SchemaRef::Inline(schema) = ok_content.schema.as_ref().unwrap() {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
        } else {
            panic!("Expected inline String schema for Ok type");
        }

        assert!(
            ok_response.headers.is_some(),
            "HeaderMap should set headers"
        );
    }

    #[test]
    fn test_parse_return_type_with_mixed_tuple_uses_last_as_body() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        // Additional tuple elements before the payload should be ignored; last element is body
        let parsed: syn::Signature =
            syn::parse_str("fn test() -> Result<(StatusCode, HeaderMap, u32, String), String>")
                .expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        let ok_response = responses.get("200").expect("Ok response missing");
        let ok_content = ok_response
            .content
            .as_ref()
            .expect("Ok content missing")
            .get("application/json")
            .expect("application/json missing");

        if let SchemaRef::Inline(schema) = ok_content.schema.as_ref().unwrap() {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
        } else {
            panic!("Expected inline String schema for Ok type");
        }

        assert!(
            ok_response.headers.is_some(),
            "HeaderMap should set headers"
        );
    }

    #[test]
    fn test_parse_return_type_primitive_types() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let test_cases = vec![
            ("-> i8", SchemaType::Integer),
            ("-> i16", SchemaType::Integer),
            ("-> i32", SchemaType::Integer),
            ("-> i64", SchemaType::Integer),
            ("-> u8", SchemaType::Integer),
            ("-> u16", SchemaType::Integer),
            ("-> u32", SchemaType::Integer),
            ("-> u64", SchemaType::Integer),
            ("-> f32", SchemaType::Number),
            ("-> f64", SchemaType::Number),
            ("-> bool", SchemaType::Boolean),
            ("-> String", SchemaType::String),
        ];

        for (return_type_str, expected_schema_type) in test_cases {
            let full_signature = format!("fn test() {}", return_type_str);
            let parsed: syn::Signature = syn::parse_str(&full_signature)
                .expect(&format!("Failed to parse return type: {}", return_type_str));

            let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

            assert_eq!(responses.len(), 1);
            let response = responses.get("200").unwrap();
            let content = response.content.as_ref().unwrap();
            let media_type = content.get("application/json").unwrap();

            if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                assert_eq!(schema.schema_type, Some(expected_schema_type));
            } else {
                panic!(
                    "Expected inline schema for primitive type: {}",
                    return_type_str
                );
            }
        }
    }

    #[test]
    fn test_parse_return_type_array() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let return_type_str = "-> Vec<String>";
        let full_signature = format!("fn test() {}", return_type_str);
        let parsed: syn::Signature =
            syn::parse_str(&full_signature).expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        assert_eq!(responses.len(), 1);
        let response = responses.get("200").unwrap();
        let content = response.content.as_ref().unwrap();
        let media_type = content.get("application/json").unwrap();

        if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert!(schema.items.is_some());
        } else {
            panic!("Expected inline array schema");
        }
    }

    #[test]
    fn test_parse_return_type_option() {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let return_type_str = "-> Option<String>";
        let full_signature = format!("fn test() {}", return_type_str);
        let parsed: syn::Signature =
            syn::parse_str(&full_signature).expect("Failed to parse return type");

        let responses = parse_return_type(&parsed.output, &known_schemas, &struct_definitions);

        assert_eq!(responses.len(), 1);
        let response = responses.get("200").unwrap();
        let content = response.content.as_ref().unwrap();
        let media_type = content.get("application/json").unwrap();

        if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
            assert_eq!(schema.nullable, Some(true));
            // Check that inner type is String
            if let Some(items) = &schema.items {
                if let SchemaRef::Inline(inner_schema) = items.as_ref() {
                    assert_eq!(inner_schema.schema_type, Some(SchemaType::String));
                }
            }
        } else {
            panic!("Expected inline nullable schema");
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
