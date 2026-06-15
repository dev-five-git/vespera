use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};

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
    schema::parse_type_to_schema_ref_with_schemas,
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
    known_schemas: &HashSet<String>,
    struct_definitions: &std::collections::HashMap<String, String>,
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
                                string_type
                                    .get_or_init(|| syn::parse_str::<Type>("String").unwrap()),
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
                        string_type.get_or_init(|| syn::parse_str::<Type>("String").unwrap()),
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                });
            }
        }
    }

    // Build HashSet once for O(1) path-param membership tests in parse_function_parameter
    let path_param_set: HashSet<String> = path_params.iter().cloned().collect();

    // Parse function parameters (skip Path extractor as we already handled it)
    for input in &sig.inputs {
        // Check if it's a request body (Json<T>)
        if let Some(body) = parse_request_body(input, known_schemas, struct_definitions) {
            request_body = Some(body);
        } else {
            // Skip Path extractor - we already handled path parameters above
            let is_path_extractor = if let FnArg::Typed(PatType { ty, .. }) = input
                && let Type::Path(type_path) = unwrap_validated_type(ty.as_ref())
                && !&type_path.path.segments.is_empty()
            {
                let segment = &type_path.path.segments.last().unwrap();
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
    }
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
            items: Some(Box::new(error_item)),
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

fn security_requirements(security: &[String]) -> Vec<HashMap<String, Vec<String>>> {
    security
        .iter()
        .map(|scheme| HashMap::from([(scheme.clone(), Vec::new())]))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rstest::rstest;
    use vespera_core::schema::{SchemaRef, SchemaType};

    use super::*;

    fn param_schema_type(param: &Parameter) -> Option<SchemaType> {
        match param.schema.as_ref()? {
            SchemaRef::Inline(schema) => schema.schema_type,
            SchemaRef::Ref(_) => None,
        }
    }

    fn build(sig_src: &str, path: &str, error_status: Option<&[u16]>) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            path,
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                error_status,
                ..OperationRouteConfig::default()
            },
        )
    }

    fn build_with_typed_responses(
        sig_src: &str,
        error_status: Option<&[u16]>,
        typed_responses: &[(u16, String)],
    ) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            "/items/{id}",
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                error_status,
                typed_responses: Some(typed_responses),
                ..OperationRouteConfig::default()
            },
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

    fn assert_body(op: &Operation, expected: Option<&ExpectedBody>) {
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
                            assert_eq!(schema.schema_type, Some(*schema_ty));
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
                        assert_eq!(schema.schema_type, Some(*schema_ty));
                    }
                    SchemaRef::Ref(_) => panic!("expected inline schema"),
                }
            }
        }
    }

    fn build_with_tags(sig_src: &str, path: &str, tags: Option<&[String]>) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            path,
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                tags,
                ..OperationRouteConfig::default()
            },
        )
    }

    fn build_with_security(sig_src: &str, path: &str, security: Option<&[String]>) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            path,
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                security,
                ..OperationRouteConfig::default()
            },
        )
    }

    fn build_with_operation_metadata(
        sig_src: &str,
        path: &str,
        operation_id: Option<&str>,
        summary: Option<&str>,
        deprecated: bool,
    ) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            path,
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                operation_id,
                summary,
                deprecated,
                ..OperationRouteConfig::default()
            },
        )
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

    #[test]
    fn test_build_operation_operation_id_override() {
        let op = build_with_operation_metadata(
            "fn my_handler() -> String",
            "/test",
            Some("getUser"),
            None,
            false,
        );
        assert_eq!(op.operation_id, Some("getUser".to_string()));
    }

    #[test]
    fn test_build_operation_summary_and_deprecated() {
        let op = build_with_operation_metadata(
            "fn my_handler() -> String",
            "/test",
            None,
            Some("Get a user"),
            true,
        );
        assert_eq!(op.summary, Some("Get a user".to_string()));
        assert_eq!(op.deprecated, Some(true));
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
    // Feature 1: declaring `error_status = [401, 402]` makes the explicit error
    // set authoritative, so the auto-inferred 400 for `Result<_, E>` is dropped
    // (400 is not among the declared codes). The 200 success response is intact.
    #[case(
        "fn create() -> Result<String, String>",
        "/create",
        Some(&[401u16, 402u16][..]),
        vec![],
        None,
        vec![
            ExpectedResp { status: "200", schema: Some(SchemaType::String) },
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
        assert_body(&op, expected_body.as_ref());
        assert_responses(&op, &expected_resps);
    }

    #[test]
    fn typed_responses_use_schema_refs_and_override_error_status() {
        let typed = vec![(404, "NotFoundError".to_string())];
        let op = build_with_typed_responses(
            "fn get() -> Result<String, String>",
            Some(&[404u16, 500u16]),
            &typed,
        );

        let response = op.responses.get("404").expect("404 response");
        let schema = response
            .content
            .as_ref()
            .and_then(|content| content.get("application/json"))
            .and_then(|media| media.schema.as_ref())
            .expect("typed schema");
        match schema {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/NotFoundError");
            }
            SchemaRef::Inline(_) => panic!("typed response must use schema ref"),
        }
        assert!(op.responses.contains_key("500"));
    }

    fn build_with_success_status(
        sig_src: &str,
        success_status: Option<u16>,
        error_status: Option<&[u16]>,
        typed_responses: Option<&[(u16, String)]>,
    ) -> Operation {
        let sig: syn::Signature = syn::parse_str(sig_src).expect("signature parse failed");
        build_operation_from_function(
            &sig,
            "/items/{id}",
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                error_status,
                typed_responses,
                success_status,
                ..OperationRouteConfig::default()
            },
        )
    }

    // ======== Feature 1: explicit error declarations suppress the auto-400 ========

    #[test]
    fn error_status_declaration_suppresses_auto_400() {
        // `Result<_, E>` infers a default 400; declaring `error_status = [500]`
        // makes the explicit error set authoritative, dropping the auto-400.
        let op = build(
            "fn create() -> Result<String, String>",
            "/create",
            Some(&[500u16]),
        );
        assert!(op.responses.contains_key("200"), "200 success is preserved");
        assert!(op.responses.contains_key("500"));
        assert!(
            !op.responses.contains_key("400"),
            "auto-400 must be suppressed when an explicit error set is declared"
        );
    }

    #[test]
    fn typed_responses_declaration_suppresses_auto_400() {
        let typed = vec![(500u16, "ServerError".to_string())];
        let op = build_with_success_status(
            "fn create() -> Result<String, (StatusCode, String)>",
            None,
            None,
            Some(&typed),
        );
        assert!(op.responses.contains_key("200"));
        assert!(op.responses.contains_key("500"));
        assert!(
            !op.responses.contains_key("400"),
            "auto-400 must be suppressed when `responses` is declared"
        );
    }

    #[test]
    fn declared_400_is_kept_via_error_status() {
        // When 400 is itself among the declared codes, it survives.
        let op = build(
            "fn create() -> Result<String, String>",
            "/create",
            Some(&[400u16, 404u16]),
        );
        assert!(
            op.responses.contains_key("400"),
            "declared 400 must be kept"
        );
        assert!(op.responses.contains_key("404"));
    }

    #[test]
    fn declared_400_is_kept_via_typed_responses() {
        let typed = vec![(400u16, "BadRequest".to_string())];
        let op = build_with_success_status(
            "fn create() -> Result<String, String>",
            None,
            None,
            Some(&typed),
        );
        assert!(
            op.responses.contains_key("400"),
            "declared 400 must be kept"
        );
    }

    #[test]
    fn no_declaration_keeps_inferred_400_backward_compatible() {
        // A plain `Result<_, E>` with no annotations keeps the inferred 400.
        let op = build("fn create() -> Result<String, String>", "/create", None);
        assert!(op.responses.contains_key("200"));
        assert!(
            op.responses.contains_key("400"),
            "without explicit declarations the inferred 400 stays (backward compatible)"
        );
    }

    // ======== Feature 2: `status = <u16>` re-keys the success response ========

    #[test]
    fn success_status_rekeys_200_and_preserves_body() {
        let op = build_with_success_status("fn create() -> String", Some(201), None, None);
        assert!(op.responses.contains_key("201"));
        assert!(!op.responses.contains_key("200"), "200 is re-keyed to 201");
        assert!(
            op.responses.get("201").unwrap().content.is_some(),
            "201 keeps the inferred body"
        );
    }

    #[test]
    fn success_status_204_drops_body() {
        let op = build_with_success_status("fn create() -> String", Some(204), None, None);
        let resp = op.responses.get("204").expect("204 response");
        assert!(
            resp.content.is_none(),
            "204 No Content must not carry a response body"
        );
        assert!(!op.responses.contains_key("200"));
    }

    #[test]
    fn success_status_204_with_error_status_yields_only_204_and_404() {
        // Mirrors the example `/error/status-code/{id}`:
        // `status = 204, error_status = [404]` on `Result<StatusCode, (StatusCode, String)>`.
        let op = build_with_success_status(
            "fn del() -> Result<StatusCode, (StatusCode, String)>",
            Some(204),
            Some(&[404u16]),
            None,
        );
        assert!(op.responses.contains_key("204"));
        assert!(op.responses.contains_key("404"));
        assert!(!op.responses.contains_key("200"), "no spurious 200");
        assert!(!op.responses.contains_key("400"), "no spurious 400");
        assert!(op.responses.get("204").unwrap().content.is_none());
    }

    #[test]
    fn success_status_200_is_noop() {
        let op = build_with_success_status("fn create() -> String", Some(200), None, None);
        assert!(op.responses.contains_key("200"));
    }

    #[test]
    fn validated_json_builds_request_body_and_422_response() {
        let op = build(
            "fn create(Validated(Json(req)): Validated<Json<CreateUser>>) -> String",
            "/users",
            None,
        );

        assert_body(
            &op,
            Some(&ExpectedBody {
                content_type: "application/json",
                schema: None,
            }),
        );
        let response = op.responses.get("422").expect("422 response present");
        assert_eq!(response.description, "Validation failed");
        let schema = response
            .content
            .as_ref()
            .and_then(|content| content.get("application/json"))
            .and_then(|media| media.schema.as_ref())
            .expect("422 json schema");
        let SchemaRef::Inline(schema) = schema else {
            panic!("validation response should be inline schema")
        };
        assert_eq!(schema.required, Some(vec!["errors".to_string()]));
        assert!(schema.properties.as_ref().unwrap().contains_key("errors"));
    }

    #[test]
    fn validated_path_uses_inner_path_type() {
        let op = build(
            "fn get(Validated(Path(id)): Validated<Path<i32>>) -> String",
            "/users/{id}",
            None,
        );

        assert_params(
            &op,
            &[ExpectedParam {
                name: "id",
                schema: Some(SchemaType::Integer),
            }],
        );
        assert!(op.responses.contains_key("422"));
    }

    #[test]
    fn duplicate_header_parameters_are_deduplicated_case_insensitively() {
        let sig: syn::Signature =
            syn::parse_str("fn traced(TypedHeader(x_trace_id): TypedHeader<XTraceId>) -> String")
                .expect("signature parse failed");
        let route_headers = vec![HeaderParam {
            name: "x-trace-id".to_string(),
            required: true,
            description: Some("Route-site duplicate".to_string()),
        }];

        let op = build_operation_from_function(
            &sig,
            "/traced",
            &HashSet::new(),
            &HashMap::new(),
            OperationRouteConfig {
                headers: Some(&route_headers),
                ..OperationRouteConfig::default()
            },
        );

        let headers: Vec<_> = op
            .parameters
            .as_ref()
            .expect("parameters present")
            .iter()
            .filter(|parameter| parameter.r#in == ParameterLocation::Header)
            .collect();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name, "x-trace-id");
    }

    #[test]
    fn typed_response_descriptions_match_status_class() {
        let typed = vec![(200, "OkBody".to_string()), (404, "NotFound".to_string())];
        let op = build_with_typed_responses("fn get() -> String", None, &typed);

        assert_eq!(
            op.responses.get("200").expect("200 response").description,
            "Successful response"
        );
        assert_eq!(
            op.responses.get("404").expect("404 response").description,
            "Error response"
        );
    }

    // ======== Tests for uncovered lines ========

    #[test]
    fn test_single_path_param_with_single_type() {
        // Test: Path<T> with single type
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
        // Test: non-Path extractor handling
        // When input is Query<T>, it should NOT be treated as Path
        let op = build(
            "fn search(Query(params): Query<QueryParams>) -> String",
            "/search",
            None,
        );

        // Test: Query params should be extended to parameters
        // But QueryParams is not in known_schemas/struct_definitions so it won't appear
        // The key is that it doesn't treat Query as a Path extractor (line 85 returns false)
        assert!(op.request_body.is_none()); // Query is not a body
    }

    #[test]
    fn test_non_path_extractor_with_state() {
        // Test: State<T> should be ignored
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
            SchemaRef::Ref(_) => panic!("expected inline schema"),
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
        // Test: multiple path params but single type
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
        // Test: non-Path extractor that generates params
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
            &HashSet::new(),
            &struct_definitions,
            OperationRouteConfig::default(),
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

    #[test]
    fn route_security_generates_requirement_objects_and_preserves_empty() {
        let bearer = vec!["bearerAuth".to_string(), "apiKey".to_string()];
        let op = build_with_security("fn secure() -> String", "/secure", Some(&bearer));
        let requirements = op.security.expect("security present");
        assert_eq!(requirements.len(), 2);
        assert!(requirements[0].contains_key("bearerAuth"));
        assert!(requirements[1].contains_key("apiKey"));

        let empty: Vec<String> = Vec::new();
        let op = build_with_security("fn public() -> String", "/public", Some(&empty));
        assert_eq!(op.security, Some(Vec::new()));
    }
}
