use crate::args::RouteArgs;

#[derive(Debug)]
pub struct RouteInfo {
    pub method: String,
    pub path: Option<String>,
    pub error_status: Option<Vec<u16>>,
}

pub fn check_route_by_meta(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(meta_list) => {
            (meta_list.path.segments.len() == 2
                && meta_list.path.segments[0].ident == "vespera"
                && meta_list.path.segments[1].ident == "route")
                || (meta_list.path.segments.len() == 1
                    && meta_list.path.segments[0].ident == "route")
        }
        syn::Meta::Path(path) => {
            (path.segments.len() == 2
                && path.segments[0].ident == "vespera"
                && path.segments[1].ident == "route")
                || (path.segments.len() == 1 && path.segments[0].ident == "route")
        }
        syn::Meta::NameValue(meta_nv) => {
            (meta_nv.path.segments.len() == 2
                && meta_nv.path.segments[0].ident == "vespera"
                && meta_nv.path.segments[1].ident == "route")
                || (meta_nv.path.segments.len() == 1 && meta_nv.path.segments[0].ident == "route")
        }
    }
}

pub fn extract_route_info(attrs: &[syn::Attribute]) -> Option<RouteInfo> {
    for attr in attrs {
        // Check if attribute path is "vespera" or "route"
        if check_route_by_meta(&attr.meta) {
            match &attr.meta {
                syn::Meta::List(meta_list) => {
                    // Try to parse as RouteArgs
                    if let Ok(route_args) = meta_list.parse_args::<RouteArgs>() {
                        let method = route_args.method.as_ref();
                        let method = method
                            .map(syn::Ident::to_string)
                            .unwrap_or("get".to_string());
                        let path = route_args.path.as_ref().map(syn::LitStr::value);

                        // Parse error_status array if present
                        let error_status = route_args.error_status.and_then(|array| {
                            let mut status_codes = Vec::new();
                            for elem in array.elems {
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
                            if status_codes.is_empty() {
                                None
                            } else {
                                Some(status_codes)
                            }
                        });

                        return Some(RouteInfo {
                            method,
                            path,
                            error_status,
                        });
                    }
                }
                // Try to parse as Meta::NameValue (e.g., #[route = "patch"])
                syn::Meta::NameValue(meta_nv) => {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit_str),
                        ..
                    }) = &meta_nv.value
                    {
                        let method_str = lit_str.value().to_lowercase();
                        match method_str.as_str() {
                            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                                return Some(RouteInfo {
                                    method: method_str,
                                    path: None,
                                    error_status: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                // Try to parse as Meta::Path (e.g., #[route])
                syn::Meta::Path(_) => {
                    return Some(RouteInfo {
                        method: "get".to_string(),
                        path: None,
                        error_status: None,
                    });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn parse_meta_from_attr(attr_str: &str) -> syn::Meta {
        // Parse attribute from string like "#[route()]" or "#[vespera::route(get)]"
        let full_code = format!("{} fn test() {{}}", attr_str);
        let file: syn::File = syn::parse_str(&full_code).expect("Failed to parse with attribute");

        // Extract the first attribute from the function
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            if let Some(attr) = fn_item.attrs.first() {
                return attr.meta.clone();
            }
        }

        panic!("Failed to extract meta from attribute: {}", attr_str);
    }

    #[rstest]
    // Valid route attributes (List meta)
    #[case("#[route()]", true)]
    #[case("#[vespera::route()]", true)]
    #[case("#[route(get)]", true)]
    #[case("#[vespera::route(get)]", true)]
    #[case("#[route(post)]", true)]
    #[case("#[vespera::route(post)]", true)]
    #[case("#[route(get, path = \"/api\")]", true)]
    #[case("#[vespera::route(get, path = \"/api\")]", true)]
    // Path meta (without parentheses) should return true
    #[case("#[route]", true)]
    #[case("#[vespera::route]", true)]
    // NameValue meta should return true
    #[case("#[route = \"get\"]", true)]
    #[case("#[vespera::route = \"get\"]", true)]
    // Invalid route attributes
    #[case("#[other()]", false)]
    #[case("#[vespera::other()]", false)]
    #[case("#[other(get)]", false)]
    #[case("#[vespera::other(get)]", false)]
    #[case("#[derive(Schema)]", false)]
    #[case("#[serde(rename_all = \"camelCase\")]", false)]
    #[case("#[test]", false)]
    // Nested paths with more than 2 segments should return false
    #[case("#[vespera::route::something]", false)]
    #[case("#[vespera::route::something()]", false)]
    fn test_check_route_by_meta(#[case] attr_str: &str, #[case] expected: bool) {
        let meta = parse_meta_from_attr(attr_str);
        let result = check_route_by_meta(&meta);
        assert_eq!(
            result, expected,
            "Failed for attribute: {}, expected: {}",
            attr_str, expected
        );
    }

    fn parse_attrs_from_code(code: &str) -> Vec<syn::Attribute> {
        let file: syn::File = syn::parse_str(code).expect("Failed to parse code");
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            return fn_item.attrs.clone();
        }
        vec![]
    }

    #[rstest]
    // Route with method only
    #[case("#[route(get)] fn test() {}", Some(("get".to_string(), None, None)))]
    #[case("#[route(post)] fn test() {}", Some(("post".to_string(), None, None)))]
    #[case("#[route(put)] fn test() {}", Some(("put".to_string(), None, None)))]
    #[case("#[route(patch)] fn test() {}", Some(("patch".to_string(), None, None)))]
    #[case("#[route(delete)] fn test() {}", Some(("delete".to_string(), None, None)))]
    #[case("#[route(head)] fn test() {}", Some(("head".to_string(), None, None)))]
    #[case("#[route(options)] fn test() {}", Some(("options".to_string(), None, None)))]
    #[case("#[vespera::route(get)] fn test() {}", Some(("get".to_string(), None, None)))]
    // Route with method and path
    #[case("#[route(get, path = \"/api\")] fn test() {}", Some(("get".to_string(), Some("/api".to_string()), None)))]
    #[case("#[route(post, path = \"/users\")] fn test() {}", Some(("post".to_string(), Some("/users".to_string()), None)))]
    #[case("#[route(get, path = \"/api/v1\")] fn test() {}", Some(("get".to_string(), Some("/api/v1".to_string()), None)))]
    // Route with method and error_status
    #[case("#[route(get, error_status = [400])] fn test() {}", Some(("get".to_string(), None, Some(vec![400]))))]
    #[case("#[route(get, error_status = [400, 404])] fn test() {}", Some(("get".to_string(), None, Some(vec![400, 404]))))]
    #[case("#[route(get, error_status = [400, 404, 500])] fn test() {}", Some(("get".to_string(), None, Some(vec![400, 404, 500]))))]
    // Route with method, path, and error_status
    #[case("#[route(get, path = \"/api\", error_status = [400])] fn test() {}", Some(("get".to_string(), Some("/api".to_string()), Some(vec![400]))))]
    #[case("#[route(post, path = \"/users\", error_status = [400, 404])] fn test() {}", Some(("post".to_string(), Some("/users".to_string()), Some(vec![400, 404]))))]
    // Route without method (defaults to "get")
    #[case("#[route()] fn test() {}", Some(("get".to_string(), None, None)))]
    #[case("#[route(path = \"/api\")] fn test() {}", Some(("get".to_string(), Some("/api".to_string()), None)))]
    // Route with Path meta (e.g., #[route])
    #[case("#[route] fn test() {}", Some(("get".to_string(), None, None)))]
    #[case("#[vespera::route] fn test() {}", Some(("get".to_string(), None, None)))]
    // Route with empty error_status array (should return None for error_status)
    #[case("#[route(get, error_status = [])] fn test() {}", Some(("get".to_string(), None, None)))]
    // NameValue format (should work now)
    #[case("#[route = \"get\"] fn test() {}", Some(("get".to_string(), None, None)))]
    #[case("#[route = \"post\"] fn test() {}", Some(("post".to_string(), None, None)))]
    #[case("#[route = \"put\"] fn test() {}", Some(("put".to_string(), None, None)))]
    #[case("#[route = \"patch\"] fn test() {}", Some(("patch".to_string(), None, None)))]
    #[case("#[route = \"delete\"] fn test() {}", Some(("delete".to_string(), None, None)))]
    #[case("#[route = \"head\"] fn test() {}", Some(("head".to_string(), None, None)))]
    #[case("#[route = \"options\"] fn test() {}", Some(("options".to_string(), None, None)))]
    #[case("#[vespera::route = \"get\"] fn test() {}", Some(("get".to_string(), None, None)))]
    // Invalid cases (should return None)
    #[case("#[other(get)] fn test() {}", None)]
    #[case("#[derive(Schema)] fn test() {}", None)]
    #[case("#[test] fn test() {}", None)]
    #[case("fn test() {}", None)]
    // Invalid method in NameValue format
    #[case("#[route = \"invalid\"] fn test() {}", None)]
    #[case("#[route = \"GET\"] fn test() {}", Some(("get".to_string(), None, None)))] // lowercase conversion
    // Multiple attributes - should find route attribute
    #[case("#[derive(Debug)] #[route(get, path = \"/api\")] #[test] fn test() {}", Some(("get".to_string(), Some("/api".to_string()), None)))]
    // Multiple route attributes - first one wins
    #[case("#[route(get, path = \"/first\")] #[route(post, path = \"/second\")] fn test() {}", Some(("get".to_string(), Some("/first".to_string()), None)))]
    fn test_extract_route_info(
        #[case] code: &str,
        #[case] expected: Option<(String, Option<String>, Option<Vec<u16>>)>,
    ) {
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);

        match expected {
            Some((exp_method, exp_path, exp_error_status)) => {
                assert!(
                    result.is_some(),
                    "Expected Some but got None for code: {}",
                    code
                );
                let route_info = result.unwrap();
                assert_eq!(
                    route_info.method, exp_method,
                    "Method mismatch for code: {}",
                    code
                );
                assert_eq!(
                    route_info.path, exp_path,
                    "Path mismatch for code: {}",
                    code
                );
                assert_eq!(
                    route_info.error_status, exp_error_status,
                    "Error status mismatch for code: {}",
                    code
                );
            }
            None => {
                assert!(
                    result.is_none(),
                    "Expected None but got Some({:?}) for code: {}",
                    result,
                    code
                );
            }
        }
    }
}
