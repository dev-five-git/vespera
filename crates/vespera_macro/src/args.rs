use crate::http::is_http_method;
use crate::metadata::HeaderParam;
use syn::{LitBool, LitInt, LitStr, bracketed};

pub struct RouteArgs {
    pub method: Option<syn::Ident>,
    pub path: Option<syn::LitStr>,
    pub error_status: Option<syn::ExprArray>,
    pub responses: Option<syn::ExprArray>,
    /// Declared non-200 success status from `status = <u16>` (validated 2xx).
    pub success_status: Option<u16>,
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
        let mut args = RouteArgsBuilder::default();

        // Parse comma-separated list of arguments
        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                // Try to parse as method identifier (get, post, etc.)
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string().to_lowercase();
                args.parse_ident(input, &ident, &ident_str, lookahead)?;

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

        Ok(args.finish())
    }
}

#[derive(Default)]
struct RouteArgsBuilder {
    method: Option<syn::Ident>,
    path: Option<syn::LitStr>,
    error_status: Option<syn::ExprArray>,
    responses: Option<syn::ExprArray>,
    success_status: Option<u16>,
    tags: Option<syn::ExprArray>,
    security: Option<syn::ExprArray>,
    headers: Option<Vec<HeaderParam>>,
    operation_id: Option<syn::LitStr>,
    summary: Option<syn::LitStr>,
    request_example: Option<syn::LitStr>,
    response_example: Option<syn::LitStr>,
    deprecated: bool,
    description: Option<syn::LitStr>,
}

impl RouteArgsBuilder {
    fn parse_ident(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
        ident_str: &str,
        lookahead: syn::parse::Lookahead1,
    ) -> syn::Result<()> {
        if is_http_method(ident_str) {
            return self.parse_method(ident);
        }
        match ident_str {
            "path" => self.parse_path(input, ident),
            "error_status" => self.parse_error_status(input, ident),
            "responses" => self.parse_responses(input, ident),
            "status" => self.parse_status(input, ident),
            "tags" => self.parse_tags(input, ident),
            "security" => self.parse_security(input, ident),
            "headers" => self.parse_headers(input, ident),
            "operation_id" => self.parse_operation_id(input, ident),
            "summary" => self.parse_summary(input, ident),
            "request_example" => self.parse_request_example(input, ident),
            "response_example" => self.parse_response_example(input, ident),
            "deprecated" => self.parse_deprecated(ident),
            "description" => self.parse_description(input, ident),
            _ => Err(lookahead.error()),
        }
    }

    fn parse_method(&mut self, ident: &syn::Ident) -> syn::Result<()> {
        reject_duplicate(self.method.as_ref(), ident, "HTTP method")?;
        self.method = Some(ident.clone());
        Ok(())
    }

    fn parse_path(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.path.as_ref(), ident, "path")?;
        input.parse::<syn::Token![=]>()?;
        self.path = Some(input.parse()?);
        Ok(())
    }

    fn parse_error_status(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.error_status.as_ref(), ident, "error_status")?;
        input.parse::<syn::Token![=]>()?;
        let array: syn::ExprArray = input.parse()?;
        validate_error_status_array(&array)?;
        self.error_status = Some(array);
        Ok(())
    }

    fn parse_responses(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.responses.as_ref(), ident, "responses")?;
        input.parse::<syn::Token![=]>()?;
        let array: syn::ExprArray = input.parse()?;
        validate_responses_array(&array)?;
        self.responses = Some(array);
        Ok(())
    }

    fn parse_status(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.success_status.as_ref(), ident, "status")?;
        input.parse::<syn::Token![=]>()?;
        let lit: LitInt = input.parse()?;
        let code = lit.base10_parse::<u16>()?;
        if !(200..300).contains(&code) {
            return Err(syn::Error::new(
                lit.span(),
                "#[route] `status` must be a 2xx success status code (200-299).",
            ));
        }
        self.success_status = Some(code);
        Ok(())
    }

    fn parse_tags(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.tags.as_ref(), ident, "tags")?;
        input.parse::<syn::Token![=]>()?;
        self.tags = Some(input.parse()?);
        Ok(())
    }

    fn parse_security(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.security.as_ref(), ident, "security")?;
        input.parse::<syn::Token![=]>()?;
        self.security = Some(input.parse()?);
        Ok(())
    }

    fn parse_headers(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.headers.as_ref(), ident, "headers")?;
        self.headers = Some(parse_header_values(input)?);
        Ok(())
    }

    fn parse_lit_str_slot(
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
        slot: &mut Option<syn::LitStr>,
        name: &str,
    ) -> syn::Result<()> {
        reject_duplicate(slot.as_ref(), ident, name)?;
        input.parse::<syn::Token![=]>()?;
        *slot = Some(input.parse()?);
        Ok(())
    }

    fn parse_operation_id(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.operation_id, "operation_id")
    }

    fn parse_summary(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.summary, "summary")
    }

    fn parse_request_example(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.request_example, "request_example")
    }

    fn parse_response_example(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.response_example, "response_example")
    }

    fn parse_deprecated(&mut self, ident: &syn::Ident) -> syn::Result<()> {
        if self.deprecated {
            return Err(duplicate_error(ident, "deprecated"));
        }
        self.deprecated = true;
        Ok(())
    }

    fn parse_description(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.description, "description")
    }

    fn finish(self) -> RouteArgs {
        RouteArgs {
            method: self.method,
            path: self.path,
            error_status: self.error_status,
            responses: self.responses,
            success_status: self.success_status,
            tags: self.tags,
            security: self.security,
            headers: self.headers,
            operation_id: self.operation_id,
            summary: self.summary,
            request_example: self.request_example,
            response_example: self.response_example,
            deprecated: self.deprecated,
            description: self.description,
        }
    }
}

fn reject_duplicate<T>(slot: Option<&T>, ident: &syn::Ident, name: &str) -> syn::Result<()> {
    if slot.is_some() {
        Err(duplicate_error(ident, name))
    } else {
        Ok(())
    }
}

fn duplicate_error(ident: &syn::Ident, name: &str) -> syn::Error {
    syn::Error::new(
        ident.span(),
        format!("#[route] `{name}` specified more than once"),
    )
}

/// Validate `error_status = [<u16>, ...]`: every element must be an integer
/// literal in the `u16` range.  A malformed entry is rejected with a
/// span-attached compile error instead of being silently dropped by the
/// downstream `filter_map` extraction (which would emit incomplete OpenAPI).
fn validate_error_status_array(array: &syn::ExprArray) -> syn::Result<()> {
    for elem in &array.elems {
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit_int),
            ..
        }) = elem
        else {
            return Err(syn::Error::new_spanned(
                elem,
                "#[route] `error_status` entries must be integer status codes, \
                 e.g. `error_status = [400, 404]`.",
            ));
        };
        lit_int.base10_parse::<u16>().map_err(|_| {
            syn::Error::new_spanned(
                lit_int,
                "#[route] `error_status` code must be in the u16 range (0-65535).",
            )
        })?;
    }
    Ok(())
}

/// Validate `responses = [(<u16>, Type), ...]`: every element must be a
/// `(status, Type)` tuple with a `u16` status literal and a type **path**.
/// Malformed entries (a bare `(404)` parenthesized expr, a wrong-arity tuple,
/// a non-integer status, or a non-path type) are rejected with a span-attached
/// compile error instead of being silently dropped by the downstream
/// `filter_map` extraction — which previously produced incomplete OpenAPI with
/// no diagnostic (e.g. `responses = [(404)]` parsed "successfully" and emitted
/// nothing).
fn validate_responses_array(array: &syn::ExprArray) -> syn::Result<()> {
    for elem in &array.elems {
        let syn::Expr::Tuple(tuple) = elem else {
            return Err(syn::Error::new_spanned(
                elem,
                "#[route] `responses` entries must be `(status, Type)` tuples, \
                 e.g. `responses = [(404, NotFoundError)]`.",
            ));
        };
        if tuple.elems.len() != 2 {
            return Err(syn::Error::new_spanned(
                tuple,
                "#[route] `responses` entry must be a `(status, Type)` tuple with \
                 exactly two elements, e.g. `(404, NotFoundError)`.",
            ));
        }
        let status = &tuple.elems[0];
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit_int),
            ..
        }) = status
        else {
            return Err(syn::Error::new_spanned(
                status,
                "#[route] `responses` status must be an integer literal, \
                 e.g. `(404, NotFoundError)`.",
            ));
        };
        lit_int.base10_parse::<u16>().map_err(|_| {
            syn::Error::new_spanned(
                lit_int,
                "#[route] `responses` status must be in the u16 range (0-65535).",
            )
        })?;
        let schema = &tuple.elems[1];
        if !matches!(schema, syn::Expr::Path(_)) {
            return Err(syn::Error::new_spanned(
                schema,
                "#[route] `responses` type must be a type path, \
                 e.g. `(404, NotFoundError)` or `(400, crate::errors::BadRequestError)`.",
            ));
        }
    }
    Ok(())
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
    let mut required_seen = false;
    let mut description: Option<String> = None;

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();
        content.parse::<syn::Token![=]>()?;

        // Reject a repeated field in one header object instead of letting the
        // later value silently win (which produced ambiguous OpenAPI with no
        // diagnostic).  `required` is a bare `bool`, so it needs its own
        // seen-flag; `name`/`description` are `Option`s already.
        match ident_str.as_str() {
            "name" => {
                if name.is_some() {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `name`",
                    ));
                }
                name = Some(content.parse::<LitStr>()?.value());
            }
            "required" => {
                if required_seen {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `required`",
                    ));
                }
                required = content.parse::<LitBool>()?.value;
                required_seen = true;
            }
            "description" => {
                if description.is_some() {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `description`",
                    ));
                }
                description = Some(content.parse::<LitStr>()?.value());
            }
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
    // Malformed error_status entries are now span-attached compile errors:
    #[case("error_status = [\"400\"]", false, None, None, None)] // not an integer
    #[case("error_status = [400, \"404\"]", false, None, None, None)] // mixed
    #[case("error_status = [70000]", false, None, None, None)] // out of u16 range
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
}
