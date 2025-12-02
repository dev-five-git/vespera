pub struct RouteArgs {
    pub method: Option<syn::Ident>,
    pub path: Option<syn::LitStr>,
    pub error_status: Option<syn::ExprArray>,
}

impl syn::parse::Parse for RouteArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut method: Option<syn::Ident> = None;
        let mut path: Option<syn::LitStr> = None;
        let mut error_status: Option<syn::ExprArray> = None;

        // Parse comma-separated list of arguments
        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                // Try to parse as method identifier (get, post, etc.)
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string().to_lowercase();
                match ident_str.as_str() {
                    "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                        method = Some(ident);
                    }
                    "path" => {
                        input.parse::<syn::Token![=]>()?;
                        let lit: syn::LitStr = input.parse()?;
                        path = Some(lit);
                    }
                    "error_status" => {
                        input.parse::<syn::Token![=]>()?;
                        let array: syn::ExprArray = input.parse()?;
                        error_status = Some(array);
                    }
                    _ => {
                        return Err(lookahead.error());
                    }
                }
            } else {
                return Err(lookahead.error());
            }

            // Check if there's a comma
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(RouteArgs {
            method,
            path,
            error_status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
                        "Expected method {} but got None for input: {}",
                        exp_method,
                        input
                    );
                    assert_eq!(
                        route_args.method.as_ref().unwrap().to_string(),
                        exp_method,
                        "Method mismatch for input: {}",
                        input
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
                        "Expected path {} but got None for input: {}",
                        exp_path,
                        input
                    );
                    assert_eq!(
                        route_args.path.as_ref().unwrap().value(),
                        exp_path,
                        "Path mismatch for input: {}",
                        input
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
                        "Expected error_status {:?} but got None for input: {}",
                        exp_status,
                        input
                    );
                    let array = route_args.error_status.as_ref().unwrap();
                    let mut status_codes = Vec::new();
                    for elem in &array.elems {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(lit_int),
                            ..
                        }) = elem
                        {
                            if let Ok(code) = lit_int.base10_parse::<u16>() {
                                status_codes.push(code);
                            }
                        }
                    }
                    assert_eq!(
                        status_codes, exp_status,
                        "Error status mismatch for input: {}",
                        input
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
                panic!(
                    "Expected successful parse but got error: {} for input: {}",
                    e, input
                );
            }
            (false, Ok(_)) => {
                panic!("Expected parse error but got success for input: {}", input);
            }
        }
    }
}
