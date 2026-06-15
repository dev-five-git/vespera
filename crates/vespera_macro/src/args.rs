use crate::http::is_http_method;
use crate::metadata::HeaderParam;
use syn::{LitBool, LitStr, bracketed};

pub struct RouteArgs {
    pub method: Option<syn::Ident>,
    pub path: Option<syn::LitStr>,
    pub error_status: Option<syn::ExprArray>,
    pub responses: Option<syn::ExprArray>,
    pub tags: Option<syn::ExprArray>,
    pub security: Option<syn::ExprArray>,
    pub headers: Option<Vec<HeaderParam>>,
    pub operation_id: Option<syn::LitStr>,
    pub summary: Option<syn::LitStr>,
    pub request_example: Option<syn::LitStr>,
    pub response_example: Option<syn::LitStr>,
    pub deprecated: bool,
    pub description: Option<syn::LitStr>,
}

impl syn::parse::Parse for RouteArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut method: Option<syn::Ident> = None;
        let mut path: Option<syn::LitStr> = None;
        let mut error_status: Option<syn::ExprArray> = None;
        let mut responses: Option<syn::ExprArray> = None;
        let mut tags: Option<syn::ExprArray> = None;
        let mut security: Option<syn::ExprArray> = None;
        let mut headers: Option<Vec<HeaderParam>> = None;
        let mut operation_id: Option<syn::LitStr> = None;
        let mut summary: Option<syn::LitStr> = None;
        let mut request_example: Option<syn::LitStr> = None;
        let mut response_example: Option<syn::LitStr> = None;
        let mut deprecated = false;
        let mut description: Option<syn::LitStr> = None;

        // Parse comma-separated list of arguments
        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                // Try to parse as method identifier (get, post, etc.)
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string().to_lowercase();
                if is_http_method(&ident_str) {
                    method = Some(ident);
                } else if ident_str == "path" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    path = Some(lit);
                } else if ident_str == "error_status" {
                    input.parse::<syn::Token![=]>()?;
                    let array: syn::ExprArray = input.parse()?;
                    error_status = Some(array);
                } else if ident_str == "responses" {
                    input.parse::<syn::Token![=]>()?;
                    let array: syn::ExprArray = input.parse()?;
                    responses = Some(array);
                } else if ident_str == "tags" {
                    input.parse::<syn::Token![=]>()?;
                    let array: syn::ExprArray = input.parse()?;
                    tags = Some(array);
                } else if ident_str == "security" {
                    input.parse::<syn::Token![=]>()?;
                    let array: syn::ExprArray = input.parse()?;
                    security = Some(array);
                } else if ident_str == "headers" {
                    headers = Some(parse_header_values(input)?);
                } else if ident_str == "operation_id" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    operation_id = Some(lit);
                } else if ident_str == "summary" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    summary = Some(lit);
                } else if ident_str == "request_example" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    request_example = Some(lit);
                } else if ident_str == "response_example" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    response_example = Some(lit);
                } else if ident_str == "deprecated" {
                    deprecated = true;
                } else if ident_str == "description" {
                    input.parse::<syn::Token![=]>()?;
                    let lit: syn::LitStr = input.parse()?;
                    description = Some(lit);
                } else {
                    return Err(lookahead.error());
                }

                // Check if there's a comma
                if input.peek(syn::Token![,]) {
                    input.parse::<syn::Token![,]>()?;
                } else {
                    break;
                }
            } else {
                return Err(lookahead.error());
            }
        }

        Ok(Self {
            method,
            path,
            error_status,
            responses,
            tags,
            security,
            headers,
            operation_id,
            summary,
            request_example,
            response_example,
            deprecated,
            description,
        })
    }
}

fn parse_header_values(input: syn::parse::ParseStream) -> syn::Result<Vec<HeaderParam>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut headers = Vec::new();

    while !content.is_empty() {
        headers.push(parse_header_struct(&content)?);

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    Ok(headers)
}

fn parse_header_struct(input: syn::parse::ParseStream) -> syn::Result<HeaderParam> {
    let content;
    syn::braced!(content in input);

    let mut name: Option<String> = None;
    let mut required = false;
    let mut description: Option<String> = None;

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();
        content.parse::<syn::Token![=]>()?;

        match ident_str.as_str() {
            "name" => name = Some(content.parse::<LitStr>()?.value()),
            "required" => required = content.parse::<LitBool>()?.value,
            "description" => description = Some(content.parse::<LitStr>()?.value()),
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!(
                        "unknown header field: `{ident_str}`. Expected `name`, `required`, or `description`"
                    ),
                ));
            }
        }

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    let name = name.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[route] headers entry missing required `name` field.",
        )
    })?;

    Ok(HeaderParam {
        name,
        required,
        description,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    // Method only
    #[case("get", true, Some("get"), None, None)]
    #[case("post", true, Some("post"), None, None)]
    #[case("put", true, Some("put"), None, None)]
    #[case("patch", true, Some("patch"), None, None)]
    #[case("delete", true, Some("delete"), None, None)]
    #[case("head", true, Some("head"), None, None)]
    #[case("options", true, Some("options"), None, None)]
    // Path only
    #[case("path = \"/api\"", true, None, Some("/api"), None)]
    #[case("path = \"/users\"", true, None, Some("/users"), None)]
    #[case("path = \"/api/v1\"", true, None, Some("/api/v1"), None)]
    // Method and path
    #[case("get, path = \"/api\"", true, Some("get"), Some("/api"), None)]
    #[case("post, path = \"/users\"", true, Some("post"), Some("/users"), None)]
    #[case("path = \"/api\", get", true, Some("get"), Some("/api"), None)]
    // Error status only
    #[case("error_status = [400]", true, None, None, Some(vec![400]))]
    #[case("error_status = [400, 404]", true, None, None, Some(vec![400, 404]))]
    #[case("error_status = [400, 404, 500]", true, None, None, Some(vec![400, 404, 500]))]
    // Method and error_status
    #[case("get, error_status = [400]", true, Some("get"), None, Some(vec![400]))]
    #[case("post, error_status = [400, 404]", true, Some("post"), None, Some(vec![400, 404]))]
    // Path and error_status
    #[case("path = \"/api\", error_status = [400]", true, None, Some("/api"), Some(vec![400]))]
    // All three
    #[case("get, path = \"/api\", error_status = [400]", true, Some("get"), Some("/api"), Some(vec![400]))]
    #[case("post, path = \"/users\", error_status = [400, 404]", true, Some("post"), Some("/users"), Some(vec![400, 404]))]
    #[case("path = \"/api\", get, error_status = [400]", true, Some("get"), Some("/api"), Some(vec![400]))]
    // Empty input
    #[case("", true, None, None, None)]
    // Invalid cases
    #[case("invalid", false, None, None, None)]
    #[case("path", false, None, None, None)]
    #[case("error_status", false, None, None, None)]
    #[case("get, invalid", false, None, None, None)]
    #[case("path =", false, None, None, None)]
    #[case("error_status =", false, None, None, None)]
    // Non-Ident tokens (should trigger line 40)
    #[case("123", false, None, None, None)]
    #[case("\"string\"", false, None, None, None)]
    #[case("=", false, None, None, None)]
    #[case("[", false, None, None, None)]
    #[case("]", false, None, None, None)]
    #[case(",", false, None, None, None)]
    #[case("get, 123", false, None, None, None)]
    #[case("get, =", false, None, None, None)]
    fn test_route_args_parse(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_method: Option<&str>,
        #[case] expected_path: Option<&str>,
        #[case] expected_error_status: Option<Vec<u16>>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => {
                // Check method
                if let Some(exp_method) = expected_method {
                    assert!(
                        route_args.method.is_some(),
                        "Expected method {exp_method} but got None for input: {input}"
                    );
                    assert_eq!(
                        route_args.method.as_ref().unwrap().to_string(),
                        exp_method,
                        "Method mismatch for input: {input}"
                    );
                } else {
                    assert!(
                        route_args.method.is_none(),
                        "Expected no method but got {:?} for input: {}",
                        route_args.method,
                        input
                    );
                }

                // Check path
                if let Some(exp_path) = expected_path {
                    assert!(
                        route_args.path.is_some(),
                        "Expected path {exp_path} but got None for input: {input}"
                    );
                    assert_eq!(
                        route_args.path.as_ref().unwrap().value(),
                        exp_path,
                        "Path mismatch for input: {input}"
                    );
                } else {
                    assert!(
                        route_args.path.is_none(),
                        "Expected no path but got {:?} for input: {}",
                        route_args.path,
                        input
                    );
                }

                // Check error_status
                if let Some(exp_status) = expected_error_status {
                    assert!(
                        route_args.error_status.is_some(),
                        "Expected error_status {exp_status:?} but got None for input: {input}"
                    );
                    let array = route_args.error_status.as_ref().unwrap();
                    let mut status_codes = Vec::new();
                    for elem in &array.elems {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(lit_int),
                            ..
                        }) = elem
                            && let Ok(code) = lit_int.base10_parse::<u16>()
                        {
                            status_codes.push(code);
                        }
                    }
                    assert_eq!(
                        status_codes, exp_status,
                        "Error status mismatch for input: {input}"
                    );
                } else {
                    assert!(
                        route_args.error_status.is_none(),
                        "Expected no error_status but got {:?} for input: {}",
                        route_args.error_status,
                        input
                    );
                }
            }
            (false, Err(_)) => {
                // Expected error, test passes
            }
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    // Tags only
    #[case("tags = [\"users\"]", true, vec!["users"])]
    #[case("tags = [\"users\", \"admin\"]", true, vec!["users", "admin"])]
    #[case("tags = [\"api\", \"v1\", \"users\"]", true, vec!["api", "v1", "users"])]
    // Tags with method
    #[case("get, tags = [\"users\"]", true, vec!["users"])]
    #[case("post, tags = [\"users\", \"create\"]", true, vec!["users", "create"])]
    // Tags with path
    #[case("path = \"/api\", tags = [\"api\"]", true, vec!["api"])]
    // Tags with method and path
    #[case("get, path = \"/users\", tags = [\"users\"]", true, vec!["users"])]
    // Empty tags array
    #[case("tags = []", true, vec![])]
    fn test_route_args_parse_tags(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_tags: Vec<&str>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => {
                if expected_tags.is_empty() {
                    // Empty array should result in Some with empty vec
                    if let Some(tags_array) = &route_args.tags {
                        assert!(tags_array.elems.is_empty());
                    }
                } else {
                    assert!(
                        route_args.tags.is_some(),
                        "Expected tags but got None for input: {input}"
                    );
                    let tags_array = route_args.tags.as_ref().unwrap();
                    let mut parsed_tags = Vec::new();
                    for elem in &tags_array.elems {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(lit_str),
                            ..
                        }) = elem
                        {
                            parsed_tags.push(lit_str.value());
                        }
                    }
                    assert_eq!(
                        parsed_tags, expected_tags,
                        "Tags mismatch for input: {input}"
                    );
                }
            }
            (false, Err(_)) => {
                // Expected error, test passes
            }
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    // Security only
    #[case("security = [\"bearerAuth\"]", true, vec!["bearerAuth"])]
    #[case("security = [\"bearerAuth\", \"apiKey\"]", true, vec!["bearerAuth", "apiKey"])]
    // Security with method/path
    #[case("get, security = [\"bearerAuth\"]", true, vec!["bearerAuth"])]
    #[case("post, path = \"/users\", security = [\"apiKey\"]", true, vec!["apiKey"])]
    // Empty security array means explicit no auth
    #[case("security = []", true, vec![])]
    fn test_route_args_parse_security(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_security: Vec<&str>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => {
                let security_array = route_args
                    .security
                    .as_ref()
                    .unwrap_or_else(|| panic!("Expected security for input: {input}"));
                let mut parsed_security = Vec::new();
                for elem in &security_array.elems {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit_str),
                        ..
                    }) = elem
                    {
                        parsed_security.push(lit_str.value());
                    }
                }
                assert_eq!(
                    parsed_security, expected_security,
                    "Security mismatch for input: {input}"
                );
            }
            (false, Err(_)) => {}
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    #[case(
        r#"headers = [{ name = "Authorization", required = true, description = "Bearer token" }, { name = "X-Trace-Id" }]"#,
        vec![
            HeaderParam { name: "Authorization".to_string(), required: true, description: Some("Bearer token".to_string()) },
            HeaderParam { name: "X-Trace-Id".to_string(), required: false, description: None },
        ]
    )]
    #[case(r"get, headers = []", vec![])]
    fn test_route_args_parse_headers(
        #[case] input: &str,
        #[case] expected_headers: Vec<HeaderParam>,
    ) {
        let route_args = syn::parse_str::<RouteArgs>(input)
            .unwrap_or_else(|e| panic!("Expected successful parse for {input}: {e}"));
        assert_eq!(route_args.headers.unwrap(), expected_headers);
    }

    #[rstest]
    #[case(r"headers = [{ required = true }]")]
    #[case(r#"headers = [{ name = "Authorization", unknown = "x" }]"#)]
    #[case(r#"headers = [{ name = "Authorization", required = "yes" }]"#)]
    fn test_route_args_parse_headers_invalid(#[case] input: &str) {
        assert!(syn::parse_str::<RouteArgs>(input).is_err());
    }

    #[rstest]
    #[case("responses = [(404, NotFoundError)]", true, vec![(404, "NotFoundError")])]
    #[case("responses = [(400, crate::errors::BadRequestError)]", true, vec![(400, "BadRequestError")])]
    #[case("get, responses = [(404, NotFoundError), (400, crate::errors::BadRequestError)]", true, vec![(404, "NotFoundError"), (400, "BadRequestError")])]
    #[case("responses", false, vec![])]
    #[case("responses = [(404)]", true, vec![])]
    fn test_route_args_parse_responses(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_responses: Vec<(u16, &str)>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => {
                let responses_array = route_args
                    .responses
                    .as_ref()
                    .unwrap_or_else(|| panic!("Expected responses for input: {input}"));
                let parsed_responses: Vec<(u16, String)> = responses_array
                    .elems
                    .iter()
                    .filter_map(|elem| {
                        let syn::Expr::Tuple(tuple) = elem else {
                            return None;
                        };
                        let status = tuple.elems.first().and_then(|status| {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Int(lit_int),
                                ..
                            }) = status
                            {
                                lit_int.base10_parse::<u16>().ok()
                            } else {
                                None
                            }
                        })?;
                        let schema_name = tuple.elems.get(1).and_then(|schema| {
                            if let syn::Expr::Path(path) = schema {
                                path.path.segments.last().map(|seg| seg.ident.to_string())
                            } else {
                                None
                            }
                        })?;
                        Some((status, schema_name))
                    })
                    .collect();
                let expected: Vec<(u16, String)> = expected_responses
                    .into_iter()
                    .map(|(status, schema)| (status, schema.to_string()))
                    .collect();
                assert_eq!(
                    parsed_responses, expected,
                    "Responses mismatch for input: {input}"
                );
            }
            (false, Err(_)) => {}
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    #[case("deprecated", true)]
    #[case("get, deprecated", true)]
    #[case("post, path = \"/users\", deprecated", true)]
    #[case("deprecated = true", false)]
    fn test_route_args_parse_deprecated(#[case] input: &str, #[case] should_parse: bool) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => assert!(route_args.deprecated),
            (false, Err(_)) => {}
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    #[case("operation_id = \"getUser\"", true, Some("getUser"))]
    #[case("get, operation_id = \"listUsers\"", true, Some("listUsers"))]
    #[case("operation_id", false, None)]
    #[case("operation_id = 123", false, None)]
    fn test_route_args_parse_operation_id(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_operation_id: Option<&str>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => assert_eq!(
                route_args.operation_id.as_ref().map(syn::LitStr::value),
                expected_operation_id.map(str::to_string)
            ),
            (false, Err(_)) => {}
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    #[case("summary = \"Get a user\"", true, Some("Get a user"))]
    #[case("get, summary = \"List users\"", true, Some("List users"))]
    #[case("summary", false, None)]
    #[case("summary = 123", false, None)]
    fn test_route_args_parse_summary(
        #[case] input: &str,
        #[case] should_parse: bool,
        #[case] expected_summary: Option<&str>,
    ) {
        let result = syn::parse_str::<RouteArgs>(input);

        match (should_parse, result) {
            (true, Ok(route_args)) => assert_eq!(
                route_args.summary.as_ref().map(syn::LitStr::value),
                expected_summary.map(str::to_string)
            ),
            (false, Err(_)) => {}
            (true, Err(e)) => {
                panic!("Expected successful parse but got error: {e} for input: {input}");
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {input}");
            }
        }
    }

    #[rstest]
    #[case(
        r#"request_example = "{\"name\":\"Alice\"}""#,
        Some(r#"{"name":"Alice"}"#),
        None
    )]
    #[case(r#"response_example = "{\"id\":1}""#, None, Some(r#"{"id":1}"#))]
    fn test_route_args_parse_examples(
        #[case] input: &str,
        #[case] expected_request: Option<&str>,
        #[case] expected_response: Option<&str>,
    ) {
        let route_args = syn::parse_str::<RouteArgs>(input).unwrap();
        assert_eq!(
            route_args.request_example.as_ref().map(syn::LitStr::value),
            expected_request.map(str::to_string)
        );
        assert_eq!(
            route_args.response_example.as_ref().map(syn::LitStr::value),
            expected_response.map(str::to_string)
        );
    }
}
