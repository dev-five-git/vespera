use std::collections::BTreeMap;

use syn::{FnArg, PatType, Type};
use vespera_core::route::{MediaType, Operation, Parameter, ParameterLocation, Response};

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
    tags: Option<&[String]>,
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
        tags: tags.map(|t| t.to_vec()),
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
        build_operation_from_function(
            &sig,
            path,
            &HashMap::new(),
            &HashMap::new(),
            error_status,
            None,
        )
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
                let media = body
                    .content
                    .get(exp.content_type)
                    .or_else(|| {
                        // allow fallback to the only available content type if expected is absent
                        if body.content.len() == 1 {
                            body.content.values().next()
                        } else {
                            None
                        }
                    })
                    .expect("expected content type");
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

    fn build_with_tags(sig_src: &str, path: &str, tags: Option<&[String]>) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(&sig, path, &HashMap::new(), &HashMap::new(), None, tags)
    }

    #[test]
    fn test_build_operation_with_tags() {
        let tags = vec!["users".to_string(), "admin".to_string()];
        let op = build_with_tags("fn test() -> String", "/test", Some(&tags));
        assert_eq!(op.tags, Some(tags));
    }

    #[test]
    fn test_build_operation_without_tags() {
        let op = build_with_tags("fn test() -> String", "/test", None);
        assert_eq!(op.tags, None);
    }

    #[test]
    fn test_build_operation_operation_id() {
        let op = build("fn my_handler() -> String", "/test", None);
        assert_eq!(op.operation_id, Some("my_handler".to_string()));
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

    // ======== Tests for uncovered lines ========

    #[test]
    fn test_single_path_param_with_single_type() {
        // Test line 55: Path<T> with single type (not tuple) and exactly ONE path param
        // This exercises the branch: path_params.len() == 1 with non-tuple type
        let op = build("fn get(Path(id): Path<i32>) -> String", "/users/{id}", None);

        // Should have exactly 1 path parameter with Integer type
        let params = op.parameters.as_ref().expect("parameters expected");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "id");
        assert_eq!(param_schema_type(&params[0]), Some(SchemaType::Integer));
    }

    #[test]
    fn test_single_path_param_with_string_type() {
        // Another test for line 55: Path<String> with single path param
        let op = build(
            "fn get(Path(id): Path<String>) -> String",
            "/users/{user_id}",
            None,
        );

        let params = op.parameters.as_ref().expect("parameters expected");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "user_id");
        assert_eq!(param_schema_type(&params[0]), Some(SchemaType::String));
    }

    #[test]
    fn test_non_path_extractor_with_query() {
        // Test lines 85, 89: non-Path extractor handling
        // When input is Query<T>, it should NOT be treated as Path
        let op = build(
            "fn search(Query(params): Query<QueryParams>) -> String",
            "/search",
            None,
        );

        // Query params should be extended to parameters (line 89)
        // But QueryParams is not in known_schemas/struct_definitions so it won't appear
        // The key is that it doesn't treat Query as a Path extractor (line 85 returns false)
        assert!(op.request_body.is_none()); // Query is not a body
    }

    #[test]
    fn test_non_path_extractor_with_state() {
        // Test lines 85, 89: State<T> should be ignored (not Path)
        let op = build(
            "fn handler(State(state): State<AppState>) -> String",
            "/handler",
            None,
        );

        // State is not a path extractor, and State params are typically ignored
        // line 85 returns false, so line 89 extends parameters (but State is usually filtered out)
        assert!(op.parameters.is_none() || op.parameters.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_string_body() {
        // String arg is handled by parse_request_body via is_string_like()
        let op = build("fn upload(content: String) -> String", "/upload", None);

        let body = op.request_body.as_ref().expect("request body expected");
        assert!(body.content.contains_key("text/plain"));
        let media = body.content.get("text/plain").unwrap();
        match media.schema.as_ref().unwrap() {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.schema_type, Some(SchemaType::String));
            }
            _ => panic!("expected inline schema"),
        }
    }

    #[test]
    fn test_str_ref_body() {
        // &str arg is handled by parse_request_body via is_string_like()
        let op = build("fn upload(content: &str) -> String", "/upload", None);

        let body = op.request_body.as_ref().expect("request body expected");
        assert!(body.content.contains_key("text/plain"));
    }

    #[test]
    fn test_string_ref_body() {
        // &String arg is handled by parse_request_body via is_string_like()
        let op = build("fn upload(content: &String) -> String", "/upload", None);

        let body = op.request_body.as_ref().expect("request body expected");
        assert!(body.content.contains_key("text/plain"));
    }

    #[test]
    fn test_non_string_arg_not_body() {
        // Non-string args don't become request body
        let op = build("fn process(count: i32) -> String", "/process", None);
        assert!(op.request_body.is_none());
    }

    #[test]
    fn test_multiple_path_params_with_single_type() {
        // Test line 57-60: multiple path params but single type - uses type for all
        let op = build(
            "fn get(Path(id): Path<String>) -> String",
            "/shops/{shop_id}/items/{item_id}",
            None,
        );

        // Both params should use String type
        let params = op.parameters.as_ref().expect("parameters expected");
        assert_eq!(params.len(), 2);
        assert_eq!(param_schema_type(&params[0]), Some(SchemaType::String));
        assert_eq!(param_schema_type(&params[1]), Some(SchemaType::String));
    }

    #[test]
    fn test_reference_to_non_path_type_not_body() {
        // &(tuple) is not string-like, no body created
        let op = build("fn process(data: &(i32, i32)) -> String", "/process", None);
        assert!(op.request_body.is_none());
    }

    #[test]
    fn test_reference_to_slice_not_body() {
        // &[T] is not string-like, no body created
        let op = build("fn process(data: &[u8]) -> String", "/process", None);
        assert!(op.request_body.is_none());
    }

    #[test]
    fn test_tuple_type_not_body() {
        // Tuple type is not string-like, no body created
        let op = build(
            "fn process(data: (i32, String)) -> String",
            "/process",
            None,
        );
        assert!(op.request_body.is_none());
    }

    #[test]
    fn test_array_type_not_body() {
        // Array type is not string-like, no body created
        let op = build("fn process(data: [u8; 4]) -> String", "/process", None);
        assert!(op.request_body.is_none());
    }

    #[test]
    fn test_non_path_extractor_generates_params_and_extends() {
        // Test lines 85, 89: non-Path extractor that DOES generate params
        // Query<T> where T is a known struct generates query parameters
        let sig: syn::Signature = syn::parse_str("fn search(Query(params): Query<SearchParams>, TypedHeader(auth): TypedHeader<Authorization>) -> String").unwrap();

        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "SearchParams".to_string(),
            "pub struct SearchParams { pub q: String }".to_string(),
        );

        let op = build_operation_from_function(
            &sig,
            "/search",
            &HashMap::new(),
            &struct_definitions,
            None,
            None,
        );

        // Query is not Path (line 85 returns false)
        // parse_function_parameter returns Some for Query<SearchParams>
        // Line 89: parameters.extend(params)
        // TypedHeader also generates a header parameter
        assert!(op.parameters.is_some());
        let params = op.parameters.unwrap();
        // Should have query param(s) and header param
        assert!(!params.is_empty());
    }
}
