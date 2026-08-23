use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};

use syn::{FnArg, PatType, Type};
use vespera_core::route::{MediaType, Operation, Parameter, ParameterLocation, Response};
use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

use crate::metadata::HeaderParam;

use super::{
    extractors::{is_validated_type, unwrap_validated_type},
    parameters::parse_function_parameter,
    path::extract_path_parameters,
    request_body::parse_request_body,
    response::parse_return_type,
    schema::parse_type_to_schema_ref,
};

#[derive(Clone, Copy, Default)]
pub struct OperationRouteConfig<'a> {
    pub error_status: Option<&'a [u16]>,
    pub typed_responses: Option<&'a [(u16, String)]>,
    /// Declared non-200 success status from `status = <u16>` (validated 2xx).
    pub success_status: Option<u16>,
    pub tags: Option<&'a [String]>,
    pub security: Option<&'a [String]>,
    pub headers: Option<&'a [HeaderParam]>,
    pub operation_id: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub request_example: Option<&'a serde_json::Value>,
    pub response_example: Option<&'a serde_json::Value>,
    pub deprecated: bool,
}

/// Build Operation from function signature
#[allow(clippy::too_many_lines)]
pub fn build_operation_from_function(
    sig: &syn::Signature,
    path: &str,
    known_schemas: &HashSet<&str>,
    struct_definitions: &std::collections::HashMap<&str, &str>,
    config: OperationRouteConfig<'_>,
) -> Operation {
    let path_params = extract_path_parameters(path);
    let mut parameters = Vec::new();
    let mut request_body = None;
    let mut path_extractor_type: Option<Type> = None;
    let mut has_validated_extractor = false;
    let string_type: OnceCell<Type> = OnceCell::new();

    // First pass: find Path<T> extractor and extract its type
    for input in &sig.inputs {
        if let FnArg::Typed(PatType { ty, .. }) = input
            && let Type::Path(type_path) = unwrap_validated_type(ty.as_ref())
        {
            has_validated_extractor |= is_validated_type(ty.as_ref());
            // A segment-less path names no extractor: skip it instead of panicking.
            if let Some(segment) = type_path.path.segments.last()
                && segment.ident == "Path"
                && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
            {
                path_extractor_type = Some(inner_ty.clone());
                break;
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
                            schema: Some(parse_type_to_schema_ref(
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
                            schema: Some(parse_type_to_schema_ref(
                                string_type.get_or_init(string_type_ast),
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
                        schema: Some(parse_type_to_schema_ref(
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
                            schema: Some(parse_type_to_schema_ref(
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
                    schema: Some(parse_type_to_schema_ref(
                        string_type.get_or_init(string_type_ast),
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                });
            }
        }
    }

    // Build HashSet once for O(1) path-param membership tests in parse_function_parameter
    let path_param_set: HashSet<&str> = path_params.iter().map(String::as_str).collect();

    // Parse function parameters (skip Path extractor as we already handled it)
    for input in &sig.inputs {
        // Check if it's a request body (Json<T>)
        if let Some(body) = parse_request_body(input, known_schemas, struct_definitions) {
            request_body = Some(body);
        } else {
            // Skip Path extractor - we already handled path parameters above
            let is_path_extractor = if let FnArg::Typed(PatType { ty, .. }) = input
                && let Type::Path(type_path) = unwrap_validated_type(ty.as_ref())
                && let Some(segment) = type_path.path.segments.last()
            {
                segment.ident == "Path"
            } else {
                false
            };

            if !is_path_extractor
                && let Some(params) = parse_function_parameter(
                    input,
                    &path_params,
                    &path_param_set,
                    known_schemas,
                    struct_definitions,
                )
            {
                parameters.extend(params);
            }
        }
    }

    if let Some(headers) = config.headers {
        parameters.extend(headers.iter().map(header_parameter));
    }
    deduplicate_header_parameters(&mut parameters);

    // Parse return type - may return multiple responses (for Result types)
    let mut responses = parse_return_type(&sig.output, known_schemas, struct_definitions);

    if let Some(example) = config.request_example
        && let Some(body) = request_body.as_mut()
    {
        for media in body.content.values_mut() {
            media.example = Some(example.clone());
        }
    }

    // Add additional error status codes from error_status attribute
    if let Some(status_codes) = config.error_status {
        // Clone the existing error response's media (its content-type AND schema)
        // for each extra status code — the content-type may be `text/plain` when
        // the error body is a bare `String`, not always `application/json`.
        let error_media = responses
            .iter()
            .find(|(code, _)| code.as_str() != "200")
            .and_then(|(_, resp)| resp.content.as_ref()?.iter().next())
            .map(|(content_type, media)| (content_type.clone(), media.schema.clone()));

        if let Some((content_type, schema)) = error_media {
            for &status_code in status_codes {
                let status_str = status_code.to_string();
                // Only add if not already present
                responses.entry(status_str).or_insert_with(|| {
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        content_type.clone(),
                        MediaType {
                            schema: schema.clone(),
                            example: None,
                            examples: None,
                        },
                    );

                    Response {
                        description: error_response_description(),
                        headers: None,
                        content: Some(err_content),
                    }
                });
            }
        }
    }

    // Add typed error responses from `responses = [(404, NotFoundError)]`.
    // These intentionally overwrite `error_status` entries for the same code.
    if let Some(typed_responses) = config.typed_responses {
        for (status_code, schema_name) in typed_responses {
            responses.insert(
                status_code.to_string(),
                typed_response(schema_name, response_description_for_status(*status_code)),
            );
        }
    }

    // Feature 1: explicit error declarations are authoritative. When a route
    // declares any explicit error response (via `responses` and/or
    // `error_status`), drop the auto-default `400` that `parse_return_type`
    // infers for `Result<_, E>` — unless `400` is itself among the declared
    // codes. The inferred success (200) response is unaffected.
    let declares_errors = config.typed_responses.is_some_and(|r| !r.is_empty())
        || config.error_status.is_some_and(|s| !s.is_empty());
    if declares_errors {
        let declares_400 = config
            .typed_responses
            .is_some_and(|typed| typed.iter().any(|(code, _)| *code == 400))
            || config
                .error_status
                .is_some_and(|codes| codes.contains(&400));
        if !declares_400 {
            responses.remove("400");
        }
    }

    if has_validated_extractor {
        responses
            .entry("422".to_string())
            .or_insert_with(validation_error_response);
    }

    if let Some(example) = config.response_example
        && let Some(response) = responses.get_mut("200")
        && let Some(content) = response.content.as_mut()
    {
        for media in content.values_mut() {
            media.example = Some(example.clone());
        }
    }

    // Feature 2: re-key the inferred success response under the declared
    // non-200 status (`status = <u16>`). No-body success statuses (204 No
    // Content, 304 Not Modified) must not carry a response body.
    if let Some(success) = config.success_status
        && success != 200
        && let Some(mut response) = responses.remove("200")
    {
        if matches!(success, 204 | 304) {
            response.content = None;
        }
        responses.insert(success.to_string(), response);
    }

    Operation {
        operation_id: config
            .operation_id
            .map(str::to_owned)
            .or_else(|| Some(sig.ident.to_string())),
        tags: config.tags.map(<[std::string::String]>::to_vec),
        summary: config.summary.map(str::to_owned),
        description: None,
        parameters: if parameters.is_empty() {
            None
        } else {
            Some(parameters)
        },
        request_body,
        responses,
        security: config.security.map(security_requirements),
        deprecated: config.deprecated.then_some(true),
        // OpenAPI 3.1 Operation Object members vespera does not populate from
        // `#[route]` (no DSL for externalDocs / callbacks / operation-level
        // servers): `None` so they are skip-serialized — output is unchanged.
        external_docs: None,
        callbacks: None,
        servers: None,
    }
}

/// `String` as a `syn::Type`, built straight from the identifier.
///
/// Replaces `syn::parse_str::<Type>("String").unwrap()`: constructing the AST
/// node is infallible, so the default path-parameter schema can never panic the
/// proc macro (and it skips a tokenize+parse round-trip).
fn string_type_ast() -> Type {
    Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path: syn::Path::from(syn::Ident::new("String", proc_macro2::Span::call_site())),
    })
}

fn header_parameter(header: &HeaderParam) -> Parameter {
    Parameter {
        name: header.name.clone(),
        r#in: ParameterLocation::Header,
        description: header.description.clone(),
        required: Some(header.required),
        schema: Some(SchemaRef::Inline(Box::new(Schema {
            schema_type: Some(SchemaType::String),
            ..Schema::default()
        }))),
        example: None,
    }
}

fn error_response_description() -> String {
    "Error response".to_string()
}

fn response_description_for_status(status_code: u16) -> String {
    if (200..300).contains(&status_code) {
        "Successful response".to_string()
    } else {
        error_response_description()
    }
}

/// Header parameters can be declared from both typed extractors and route-site
/// `headers = [...]`. Keep the first occurrence (signature-derived parameters
/// are appended before route-site headers and usually carry the richer schema)
/// and drop later duplicates using HTTP's case-insensitive header-name rules.
fn deduplicate_header_parameters(parameters: &mut Vec<Parameter>) {
    let mut seen_headers = HashSet::new();
    parameters.retain(|parameter| {
        if parameter.r#in != ParameterLocation::Header {
            return true;
        }
        seen_headers.insert(parameter.name.to_ascii_lowercase())
    });
}

fn typed_response(schema_name: &str, description: String) -> Response {
    let mut content = BTreeMap::new();
    content.insert(
        "application/json".to_string(),
        MediaType {
            schema: Some(SchemaRef::Ref(Reference::schema(schema_name))),
            example: None,
            examples: None,
        },
    );

    Response {
        description,
        headers: None,
        content: Some(content),
    }
}

fn validation_error_response() -> Response {
    let mut error_properties = BTreeMap::new();
    error_properties.insert(
        "path".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    error_properties.insert(
        "message".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );

    let error_item = SchemaRef::Inline(Box::new(Schema {
        schema_type: Some(SchemaType::Object),
        properties: Some(error_properties),
        required: Some(vec!["path".to_string(), "message".to_string()]),
        ..Schema::default()
    }));

    let mut response_properties = BTreeMap::new();
    response_properties.insert(
        "errors".to_string(),
        SchemaRef::Inline(Box::new(Schema {
            schema_type: Some(SchemaType::Array),
            items: Some(error_item),
            ..Schema::default()
        })),
    );

    let mut content = BTreeMap::new();
    content.insert(
        "application/json".to_string(),
        MediaType {
            schema: Some(SchemaRef::Inline(Box::new(Schema {
                schema_type: Some(SchemaType::Object),
                properties: Some(response_properties),
                required: Some(vec!["errors".to_string()]),
                ..Schema::default()
            }))),
            example: None,
            examples: None,
        },
    );

    Response {
        description: "Validation failed".to_string(),
        headers: None,
        content: Some(content),
    }
}

fn security_requirements(security: &[String]) -> Vec<BTreeMap<String, Vec<String>>> {
    security
        .iter()
        .map(|scheme| BTreeMap::from([(scheme.clone(), Vec::new())]))
        .collect()
}

#[cfg(test)]
mod tests;
