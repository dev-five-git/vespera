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
// Malformed error_status entries are now span-attached compile errors:
#[case("error_status = [\"400\"]", false, None, None, None)] // not an integer
#[case("error_status = [400, \"404\"]", false, None, None, None)] // mixed
#[case("error_status = [70000]", false, None, None, None)] // out of u16 range
#[case("error_status = [1000]", false, None, None, None)] // not an HTTP status
#[case("error_status = [99]", false, None, None, None)] // below 100
#[case("error_status = [600]", false, None, None, None)] // above 599
#[case("error_status = [100, 599]", true, None, None, Some(vec![100, 599]))] // bounds
#[case("error_status = [NotFound]", false, None, None, None)] // path, not int
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
fn test_route_args_parse_headers(#[case] input: &str, #[case] expected_headers: Vec<HeaderParam>) {
    let route_args = syn::parse_str::<RouteArgs>(input)
        .unwrap_or_else(|e| panic!("Expected successful parse for {input}: {e}"));
    assert_eq!(route_args.headers.unwrap(), expected_headers);
}

#[rstest]
#[case(r"headers = [{ required = true }]")]
#[case(r#"headers = [{ name = "Authorization", unknown = "x" }]"#)]
#[case(r#"headers = [{ name = "Authorization", required = "yes" }]"#)]
#[case(r#"headers = [{ name = "A", name = "B" }]"#)]
#[case(r#"headers = [{ name = "A", required = true, required = false }]"#)]
#[case(r#"headers = [{ name = "A", description = "x", description = "y" }]"#)]
fn test_route_args_parse_headers_invalid(#[case] input: &str) {
    assert!(syn::parse_str::<RouteArgs>(input).is_err());
}

#[rstest]
#[case("responses = [(404, NotFoundError)]", true, vec![(404, "NotFoundError")])]
#[case("responses = [(400, crate::errors::BadRequestError)]", true, vec![(400, "BadRequestError")])]
#[case("get, responses = [(404, NotFoundError), (400, crate::errors::BadRequestError)]", true, vec![(404, "NotFoundError"), (400, "BadRequestError")])]
#[case("responses", false, vec![])]
// Malformed entries are now a span-attached compile error (previously parsed
// "successfully" and silently emitted no response):
#[case("responses = [(404)]", false, vec![])] // bare paren expr, missing Type
#[case("responses = [(404, NotFoundError, Extra)]", false, vec![])] // wrong arity
#[case("responses = [404]", false, vec![])] // not a tuple
#[case("responses = [(\"404\", NotFoundError)]", false, vec![])] // status not int
#[case("responses = [(404, \"NotFoundError\")]", false, vec![])] // type not a path
#[case("responses = [(70000, NotFoundError)]", false, vec![])] // status out of u16
#[case("responses = []", true, vec![])] // empty is valid (no entries)
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
#[case("deprecated, deprecated", false)]
#[case("path = \"/a\", path = \"/b\"", false)]
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

#[rstest]
// Valid 2xx success statuses
#[case("status = 200", true, Some(200))]
#[case("status = 201", true, Some(201))]
#[case("status = 204", true, Some(204))]
#[case("status = 299", true, Some(299))]
#[case("get, status = 204", true, Some(204))]
#[case("delete, path = \"/x\", status = 204", true, Some(204))]
// Non-2xx status codes are rejected with a compile error
#[case("status = 199", false, None)]
#[case("status = 300", false, None)]
#[case("status = 404", false, None)]
#[case("status = 500", false, None)]
// Malformed: missing value / non-integer / out of u16 range
#[case("status", false, None)]
#[case("status =", false, None)]
#[case("status = \"204\"", false, None)]
#[case("status = 70000", false, None)]
fn test_route_args_parse_status(
    #[case] input: &str,
    #[case] should_parse: bool,
    #[case] expected_status: Option<u16>,
) {
    let result = syn::parse_str::<RouteArgs>(input);
    match (should_parse, result) {
        (true, Ok(route_args)) => {
            assert_eq!(
                route_args.success_status, expected_status,
                "status mismatch for input: {input}"
            );
        }
        (false, Err(_)) => {}
        (true, Err(e)) => {
            panic!("Expected successful parse but got error: {e} for input: {input}")
        }
        (false, Ok(_)) => panic!("Expected parse error but got success for input: {input}"),
    }
}

#[test]
fn empty_route_arguments_use_all_defaults() {
    let route_args = syn::parse_str::<RouteArgs>("").expect("empty route arguments parse");

    assert!(route_args.method.is_none());
    assert!(route_args.path.is_none());
    assert!(route_args.error_status.is_none());
    assert!(route_args.responses.is_none());
    assert!(route_args.success_status.is_none());
    assert!(route_args.tags.is_none());
    assert!(route_args.security.is_none());
    assert!(route_args.headers.is_none());
    assert!(route_args.operation_id.is_none());
    assert!(route_args.summary.is_none());
    assert!(route_args.request_example.is_none());
    assert!(route_args.response_example.is_none());
    assert!(!route_args.deprecated);
    assert!(route_args.description.is_none());
}

/// An unknown `#[route]` key names itself and the accepted keys instead of
/// syn's generic "expected identifier".
#[rstest]
#[case::unknown_key("route_unknown_key", "get, tag = \"users\"")]
#[case::unknown_key_with_value_list("route_unknown_key_list", "paths = [\"/a\"]")]
fn unknown_route_argument_lists_the_accepted_keys(#[case] name: &str, #[case] input: &str) {
    let Err(err) = syn::parse_str::<RouteArgs>(input) else {
        panic!("`{input}` must be rejected");
    };
    insta::assert_snapshot!(name, err.to_string());
}
