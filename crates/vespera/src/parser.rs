//! Parser module for analyzing function signatures and converting to OpenAPI structures

use syn::{FnArg, Pat, PatType, Type, ReturnType};
use vespera_core::{
    route::{Operation, Parameter, ParameterLocation, RequestBody, Response, MediaType},
    schema::{Schema, SchemaRef, SchemaType},
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
) -> Option<Parameter> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { pat, ty, .. }) => {
            let param_name = match pat.as_ref() {
                Pat::Ident(ident) => ident.ident.to_string(),
                _ => return None,
            };

            // Check if it's a path parameter
            if path_params.contains(&param_name) {
                return Some(Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref(ty)),
                    example: None,
                });
            }

            // Check for common Axum extractors
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if path.segments.is_empty() {
                    return None;
                }

                let segment = &path.segments[0];
                let ident_str = segment.ident.to_string();

                match ident_str.as_str() {
                    "Path" => {
                        // Path<T> extractor
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                return Some(Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Path,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty)),
                                    example: None,
                                });
                            }
                        }
                    }
                    "Query" => {
                        // Query<T> extractor
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                return Some(Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Query,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty)),
                                    example: None,
                                });
                            }
                        }
                    }
                    "Header" => {
                        // Header<T> extractor
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                return Some(Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Header,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref(inner_ty)),
                                    example: None,
                                });
                            }
                        }
                    }
                    "Json" => {
                        // Json<T> extractor - this will be handled as RequestBody
                        return None;
                    }
                    _ => {
                        // Check if it's a primitive type (direct parameter)
                        if is_primitive_type(ty) {
                            return Some(Parameter {
                                name: param_name.clone(),
                                r#in: ParameterLocation::Query,
                                description: None,
                                required: Some(true),
                                schema: Some(parse_type_to_schema_ref(ty)),
                                example: None,
                            });
                        }
                    }
                }
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
                    "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32"
                        | "f64" | "bool" | "String" | "str"
                )
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Parse Rust type to OpenAPI SchemaRef
pub fn parse_type_to_schema_ref(ty: &Type) -> SchemaRef {
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
                            let inner_schema = parse_type_to_schema_ref(inner_ty);
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
                _ => {
                    // For custom types, create a reference
                    // This will be resolved later when we have schema registry
                    SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                }
            }
        }
        Type::Reference(type_ref) => {
            // Handle &T, &mut T, etc.
            parse_type_to_schema_ref(&type_ref.elem)
        }
        _ => SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
    }
}

/// Analyze function signature and extract RequestBody
pub fn parse_request_body(arg: &FnArg) -> Option<RequestBody> {
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

                if ident_str == "Json" {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let schema = parse_type_to_schema_ref(inner_ty);
                            let mut content = std::collections::HashMap::new();
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
                }
            }
            None
        }
    }
}

/// Analyze return type and convert to Response
pub fn parse_return_type(return_type: &ReturnType) -> Response {
    let schema = match return_type {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some(parse_type_to_schema_ref(ty)),
    };

    let mut content = std::collections::HashMap::new();
    if let Some(schema) = schema {
        content.insert(
            "application/json".to_string(),
            MediaType {
                schema: Some(schema),
                example: None,
                examples: None,
            },
        );
    }

    Response {
        description: "Successful response".to_string(),
        headers: None,
        content: if content.is_empty() { None } else { Some(content) },
    }
}

/// Build Operation from function signature
pub fn build_operation_from_function(
    sig: &syn::Signature,
    path: &str,
) -> Operation {
    let path_params = extract_path_parameters(path);
    let mut parameters = Vec::new();
    let mut request_body = None;

    // Parse function parameters
    for input in &sig.inputs {
        // Check if it's a request body (Json<T>)
        if let Some(body) = parse_request_body(input) {
            request_body = Some(body);
        } else if let Some(param) = parse_function_parameter(input, &path_params) {
            parameters.push(param);
        }
    }

    // Parse return type
    let response = parse_return_type(&sig.output);
    let mut responses = std::collections::HashMap::new();
    responses.insert("200".to_string(), response);

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

