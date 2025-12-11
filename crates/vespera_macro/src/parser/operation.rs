use std::collections::BTreeMap;

use syn::{FnArg, PatType, Type};
use vespera_core::{
    route::{MediaType, Operation, Parameter, ParameterLocation, RequestBody, Response},
    schema::{Schema, SchemaRef},
};

use super::{
    parameters::parse_function_parameter, path::extract_path_parameters,
    request_body::parse_request_body, response::parse_return_type,
    schema::parse_type_to_schema_ref_with_schemas,
};

/// Build Operation from function signature
pub fn build_operation_from_function(
    sig: &syn::Signature,
    path: &str,
    known_schemas: &std::collections::HashMap<String, String>,
    struct_definitions: &std::collections::HashMap<String, String>,
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
            let is_path_extractor = if let FnArg::Typed(PatType { ty, .. }) = input
                && let Type::Path(type_path) = ty.as_ref()
                && !&type_path.path.segments.is_empty()
            {
                let segment = &type_path.path.segments.last().unwrap();
                segment.ident == "Path"
            } else {
                false
            };

            if !is_path_extractor
                && let Some(params) =
                    parse_function_parameter(input, &path_params, known_schemas, struct_definitions)
            {
                parameters.extend(params);
            }
        }
    }

    // Fallback: if last arg is String/&str and no body yet, treat as text/plain body
    if request_body.is_none()
        && let Some(FnArg::Typed(PatType { ty, .. })) = sig.inputs.last()
    {
        let is_string = match ty.as_ref() {
            Type::Path(type_path) => type_path
                .path
                .segments
                .last()
                .map(|s| s.ident == "String" || s.ident == "str")
                .unwrap_or(false),
            Type::Reference(type_ref) => {
                if let Type::Path(p) = type_ref.elem.as_ref() {
                    p.path
                        .segments
                        .last()
                        .map(|s| s.ident == "String" || s.ident == "str")
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        };

        if is_string {
            let mut content = BTreeMap::new();
            content.insert(
                "text/plain".to_string(),
                MediaType {
                    schema: Some(SchemaRef::Inline(Box::new(Schema::string()))),
                    example: None,
                    examples: None,
                },
            );
            request_body = Some(RequestBody {
                description: None,
                content,
                required: Some(true),
            });
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
    use vespera_core::schema::{SchemaRef, SchemaType};

    fn param_schema_type(param: &Parameter) -> Option<SchemaType> {
        match param.schema.as_ref()? {
            SchemaRef::Inline(schema) => schema.schema_type.clone(),
            SchemaRef::Ref(_) => None,
        }
    }

    fn build(sig_src: &str, path: &str, error_status: Option<&[u16]>) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(&sig, path, &HashMap::new(), &HashMap::new(), error_status)
    }

    #[derive(Clone, Debug)]
    struct ExpectedParam {
        name: &'static str,
        schema: Option<SchemaType>,
    }

    #[derive(Clone, Debug)]
    struct ExpectedBody {
        content_type: &'static str,
        schema: Option<SchemaType>,
    }

    #[derive(Clone, Debug)]
    struct ExpectedResp {
        status: &'static str,
        schema: Option<SchemaType>,
    }

    fn assert_body(op: &Operation, expected: &Option<ExpectedBody>) {
        match expected {
            None => assert!(op.request_body.is_none()),
            Some(exp) => {
                let body = op.request_body.as_ref().expect("request body expected");
                let media = body.content.get(exp.content_type).or_else(|| {
                    // allow fallback to the only available content type if expected is absent
                    if body.content.len() == 1 {
                        body.content.values().next()
                    } else {
                        None
                    }
                }).expect("expected content type");
                if let Some(schema_ty) = &exp.schema {
                    match media.schema.as_ref().expect("schema expected") {
                        SchemaRef::Inline(schema) => {
                            assert_eq!(schema.schema_type, Some(schema_ty.clone()));
                        }
                        SchemaRef::Ref(_) => panic!("expected inline schema"),
                    }
                }
            }
        }
    }

    fn assert_params(op: &Operation, expected: &[ExpectedParam]) {
        match op.parameters.as_ref() {
            None => assert!(expected.is_empty()),
            Some(params) => {
                assert_eq!(params.len(), expected.len());
                for (param, exp) in params.iter().zip(expected) {
                    assert_eq!(param.name, exp.name);
                    assert_eq!(param_schema_type(param), exp.schema);
                }
            }
        }
    }

    fn assert_responses(op: &Operation, expected: &[ExpectedResp]) {
        for exp in expected {
            let resp = op.responses.get(exp.status).expect("response missing");
            let media = resp
                .content
                .as_ref()
                .and_then(|c| c.get("application/json"))
                .or_else(|| resp.content.as_ref().and_then(|c| c.get("text/plain")))
                .expect("media type missing");
            if let Some(schema_ty) = &exp.schema {
                match media.schema.as_ref().expect("schema expected") {
                    SchemaRef::Inline(schema) => {
                        assert_eq!(schema.schema_type, Some(schema_ty.clone()));
                    }
                    SchemaRef::Ref(_) => panic!("expected inline schema"),
                }
            }
        }
    }

    #[rstest]
    #[case(
        "fn upload(data: String) -> String",
        "/upload",
        None::<&[u16]>,
        vec![],
        Some(ExpectedBody { content_type: "text/plain", schema: Some(SchemaType::String) }),
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn upload_ref(data: &str) -> String",
        "/upload",
        None::<&[u16]>,
        vec![],
        Some(ExpectedBody { content_type: "text/plain", schema: Some(SchemaType::String) }),
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn get(Path(params): Path<(i32,)>) -> String",
        "/users/{id}/{name}",
        None::<&[u16]>,
        vec![
            ExpectedParam { name: "id", schema: Some(SchemaType::Integer) },
            ExpectedParam { name: "name", schema: Some(SchemaType::String) },
        ],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn get() -> String",
        "/items/{item_id}",
        None::<&[u16]>,
        vec![ExpectedParam { name: "item_id", schema: Some(SchemaType::String) }],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn get(Path(id): Path<String>) -> String",
        "/shops/{shop_id}/items/{item_id}",
        None::<&[u16]>,
        vec![
            ExpectedParam { name: "shop_id", schema: Some(SchemaType::String) },
            ExpectedParam { name: "item_id", schema: Some(SchemaType::String) },
        ],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn create(Json(body): Json<User>) -> Result<String, String>",
        "/create",
        None::<&[u16]>,
        vec![],
        Some(ExpectedBody { content_type: "application/json", schema: None }),
        vec![
            ExpectedResp { status: "200", schema: Some(SchemaType::String) },
            ExpectedResp { status: "400", schema: Some(SchemaType::String) },
        ]
    )]
    #[case(
        "fn get(Path(params): Path<(i32,)>) -> String",
        "/users/{id}/{name}/{extra}",
        None::<&[u16]>,
        vec![
            ExpectedParam { name: "id", schema: Some(SchemaType::Integer) },
            ExpectedParam { name: "name", schema: Some(SchemaType::String) },
            ExpectedParam { name: "extra", schema: Some(SchemaType::String) },
        ],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn get() -> String",
        "/items/{item_id}/extra/{more}",
        None::<&[u16]>,
        vec![
            ExpectedParam { name: "item_id", schema: Some(SchemaType::String) },
            ExpectedParam { name: "more", schema: Some(SchemaType::String) },
        ],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn post(data: String) -> String",
        "/post",
        None::<&[u16]>,
        vec![],
        Some(ExpectedBody { content_type: "text/plain", schema: Some(SchemaType::String) }),
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn no_error_extra() -> String",
        "/plain",
        Some(&[500u16][..]),
        vec![],
        None,
        vec![ExpectedResp { status: "200", schema: Some(SchemaType::String) }]
    )]
    #[case(
        "fn create() -> Result<String, String>",
        "/create",
        Some(&[400u16, 500u16][..]),
        vec![],
        None,
        vec![
            ExpectedResp { status: "200", schema: Some(SchemaType::String) },
            ExpectedResp { status: "400", schema: Some(SchemaType::String) },
            ExpectedResp { status: "500", schema: Some(SchemaType::String) },
        ]
    )]
    #[case(
        "fn create() -> Result<String, String>",
        "/create",
        Some(&[401u16, 402u16][..]),
        vec![],
        None,
        vec![
            ExpectedResp { status: "200", schema: Some(SchemaType::String) },
            ExpectedResp { status: "400", schema: Some(SchemaType::String) },
            ExpectedResp { status: "401", schema: Some(SchemaType::String) },
            ExpectedResp { status: "402", schema: Some(SchemaType::String) },
        ]
    )]
    fn test_build_operation_cases(
        #[case] sig_src: &str,
        #[case] path: &str,
        #[case] extra_status: Option<&[u16]>,
        #[case] expected_params: Vec<ExpectedParam>,
        #[case] expected_body: Option<ExpectedBody>,
        #[case] expected_resps: Vec<ExpectedResp>,
    ) {
        let op = build(sig_src, path, extra_status);
        assert_params(&op, &expected_params);
        assert_body(&op, &expected_body);
        assert_responses(&op, &expected_resps);
    }
}
