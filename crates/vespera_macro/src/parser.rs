//! Parser module for analyzing function signatures and converting to OpenAPI structures

use std::collections::{BTreeMap, HashMap};
use syn::{Fields, FnArg, Pat, PatType, ReturnType, Type};
use vespera_core::{
    route::{MediaType, Operation, Parameter, ParameterLocation, RequestBody, Response},
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

/// Analyze function parameter and convert to OpenAPI Parameter
pub fn parse_function_parameter(
    arg: &FnArg,
    path_params: &[String],
    known_schemas: &HashMap<String, String>,
) -> Option<Parameter> {
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
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if !path.segments.is_empty() {
                    let segment = &path.segments[0];
                    let ident_str = segment.ident.to_string();

                    match ident_str.as_str() {
                        "Path" => {
                            // Path<T> extractor - use path parameter name from route if available
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                // If there's exactly one path parameter, use its name
                                let name = if path_params.len() == 1 {
                                    path_params[0].clone()
                                } else {
                                    // Otherwise use the parameter name from the pattern
                                    param_name
                                };
                                return Some(Parameter {
                                    name,
                                    r#in: ParameterLocation::Path,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty, known_schemas)),
                                    example: None,
                                });
                            }
                        }
                        "Query" => {
                            // Query<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                return Some(Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Query,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty, known_schemas)),
                                    example: None,
                                });
                            }
                        }
                        "Header" => {
                            // Header<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                return Some(Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Header,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty, known_schemas)),
                                    example: None,
                                });
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
                return Some(Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref(ty, known_schemas)),
                    example: None,
                });
            }

            // Check if it's a primitive type (direct parameter)
            if is_primitive_type(ty.as_ref()) {
                return Some(Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Query,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref(ty, known_schemas)),
                    example: None,
                });
            }

            None
        }
    }
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
        _ => field_name.to_string(),
    }
}

/// Parse struct definition to OpenAPI Schema
pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashMap<String, String>,
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
                let schema_ref = parse_type_to_schema_ref(field_type, known_schemas);

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

/// Parse Rust type to OpenAPI SchemaRef
pub fn parse_type_to_schema_ref(ty: &Type, known_schemas: &HashMap<String, String>) -> SchemaRef {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.is_empty() {
                return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
            }

            let segment = &path.segments[0];
            let ident_str = segment.ident.to_string();

            // Handle generic types
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                match ident_str.as_str() {
                    "Vec" | "Option" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner_schema = parse_type_to_schema_ref(inner_ty, known_schemas);
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
                "HashMap" | "BTreeMap" | "Vec" | "Option" | "Result" | "Json" | "Path"
                | "Query" | "Header" => {
                    // These are not schema types, return object schema
                    SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                }
                _ => {
                    // Check if this is a known schema (struct with Schema derive)
                    if known_schemas.contains_key(&ident_str) {
                        SchemaRef::Ref(Reference::schema(&ident_str))
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
            parse_type_to_schema_ref(&type_ref.elem, known_schemas)
        }
        _ => SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
    }
}

/// Analyze function signature and extract RequestBody
pub fn parse_request_body(
    arg: &FnArg,
    known_schemas: &HashMap<String, String>,
) -> Option<RequestBody> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { ty, .. }) => {
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if path.segments.is_empty() {
                    return None;
                }

                let segment = &path.segments[0];
                let ident_str = segment.ident.to_string();

                if ident_str == "Json"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                {
                    let schema = parse_type_to_schema_ref(inner_ty, known_schemas);
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
fn unwrap_json(ty: &Type) -> &Type {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            let segment = &path.segments[0];
            if segment.ident == "Json" {
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return inner_ty;
                    }
                }
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
        if let Some(segment) = path.segments.last() {
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                if args.args.len() >= 2 {
                    if let (
                        Some(syn::GenericArgument::Type(ok_ty)),
                        Some(syn::GenericArgument::Type(err_ty)),
                    ) = (args.args.first(), args.args.get(1))
                    {
                        // Unwrap Json from Ok type if present
                        let ok_ty_unwrapped = unwrap_json(ok_ty);
                        return Some((ok_ty_unwrapped.clone(), err_ty.clone()));
                    }
                }
            }
        }
    }
    None
}

/// Check if error type is a tuple (StatusCode, E) or (StatusCode, Json<E>)
/// Returns the error type E and a default status code (400)
fn extract_status_code_tuple(err_ty: &Type) -> Option<(u16, Type)> {
    if let Type::Tuple(tuple) = err_ty {
        if tuple.elems.len() == 2 {
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
    }
    None
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashMap<String, String>,
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
                let ok_schema = parse_type_to_schema_ref(&ok_ty, known_schemas);
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
                        headers: None,
                        content: Some(ok_content),
                    },
                );

                // Handle error response
                // Check if error is (StatusCode, E) tuple
                if let Some((status_code, error_type)) = extract_status_code_tuple(&err_ty) {
                    // Use the status code from the tuple
                    let err_schema = parse_type_to_schema_ref(&error_type, known_schemas);
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
                    let err_schema = parse_type_to_schema_ref(err_ty_unwrapped, known_schemas);
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
                let schema = parse_type_to_schema_ref(ty, known_schemas);
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
    error_status: Option<&[u16]>,
) -> Operation {
    let path_params = extract_path_parameters(path);
    let mut parameters = Vec::new();
    let mut request_body = None;

    // Parse function parameters
    for input in &sig.inputs {
        // Check if it's a request body (Json<T>)
        if let Some(body) = parse_request_body(input, known_schemas) {
            request_body = Some(body);
        } else if let Some(param) = parse_function_parameter(input, &path_params, known_schemas) {
            parameters.push(param);
        }
    }

    // Parse return type - may return multiple responses (for Result types)
    let mut responses = parse_return_type(&sig.output, known_schemas);

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
                if !responses.contains_key(&status_str) {
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(schema.clone()),
                            example: None,
                            examples: None,
                        },
                    );

                    responses.insert(
                        status_str,
                        Response {
                            description: "Error response".to_string(),
                            headers: None,
                            content: Some(err_content),
                        },
                    );
                }
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
