use crate::{args::RouteArgs, http::is_http_method, metadata::HeaderParam};

// Re-export the canonical `extract_doc_comment` implementation from the
// parser/schema/serde_attrs module so `crate::route::extract_doc_comment`
// (via `route/mod.rs`'s `pub use utils::*;`) continues to resolve for every
// production caller — `route_impl.rs` (route-attribute description fallback)
// and `collector.rs` (slow-path description fallback) — without holding a
// second byte-identical copy of the function.
pub use crate::parser::schema::extract_doc_comment;

#[derive(Debug)]
pub struct RouteInfo {
    pub method: String,
    pub path: Option<String>,
    pub success_status: Option<u16>,
    pub error_status: Option<Vec<u16>>,
    pub typed_responses: Option<Vec<(u16, String)>>,
    pub tags: Option<Vec<String>>,
    pub security: Option<Vec<String>>,
    pub headers: Vec<HeaderParam>,
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub request_example: Option<serde_json::Value>,
    pub response_example: Option<serde_json::Value>,
    pub deprecated: bool,
    pub description: Option<String>,
}

/// Convert a parsed [`RouteArgs`] into the simpler [`RouteInfo`] used by
/// the collector / OpenAPI emitter.  Factored out so the inline conversion
/// gets its own basic block and shows up cleanly in coverage reports.
///
/// The `path` / `description` extraction uses `if let` instead of
/// `Option::map(...)` so the unwrap branch is attributed to a source
/// line rather than an internal closure call site that LLVM coverage
/// reports as zero hits even when the field is `Some`.
#[allow(clippy::manual_map, clippy::option_if_let_else)]
fn build_route_info_from_args(route_args: &RouteArgs) -> RouteInfo {
    let method = route_args
        .method
        .as_ref()
        .map_or_else(|| "get".to_string(), syn::Ident::to_string);
    let path = if let Some(lit) = route_args.path.as_ref() {
        Some(lit.value())
    } else {
        None
    };

    let error_status = route_args
        .error_status
        .as_ref()
        .and_then(extract_status_codes);
    let tags = route_args.tags.as_ref().and_then(extract_non_empty_strings);
    let typed_responses = route_args
        .responses
        .as_ref()
        .and_then(extract_typed_responses);
    let security = route_args.security.as_ref().map(extract_strings);
    let headers = route_args.headers.clone().unwrap_or_default();

    let description = if let Some(lit) = route_args.description.as_ref() {
        Some(lit.value())
    } else {
        None
    };

    let operation_id = if let Some(lit) = route_args.operation_id.as_ref() {
        Some(lit.value())
    } else {
        None
    };

    let summary = if let Some(lit) = route_args.summary.as_ref() {
        Some(lit.value())
    } else {
        None
    };

    let request_example = route_args
        .request_example
        .as_ref()
        .map(parse_example_string);
    let response_example = route_args
        .response_example
        .as_ref()
        .map(parse_example_string);

    RouteInfo {
        method,
        path,
        success_status: route_args.success_status,
        error_status,
        typed_responses,
        tags,
        security,
        headers,
        operation_id,
        summary,
        request_example,
        response_example,
        deprecated: route_args.deprecated,
        description,
    }
}

fn parse_example_string(lit: &syn::LitStr) -> serde_json::Value {
    let value = lit.value();
    serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
}

fn extract_status_codes(array: &syn::ExprArray) -> Option<Vec<u16>> {
    let status_codes: Vec<u16> = array
        .elems
        .iter()
        .filter_map(|elem| {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(lit_int),
                ..
            }) = elem
            {
                lit_int.base10_parse::<u16>().ok()
            } else {
                None
            }
        })
        .collect();
    (!status_codes.is_empty()).then_some(status_codes)
}

fn extract_strings(array: &syn::ExprArray) -> Vec<String> {
    array
        .elems
        .iter()
        .filter_map(|elem| {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = elem
            {
                Some(lit_str.value())
            } else {
                None
            }
        })
        .collect()
}

fn extract_non_empty_strings(array: &syn::ExprArray) -> Option<Vec<String>> {
    let values = extract_strings(array);
    (!values.is_empty()).then_some(values)
}

fn extract_typed_responses(array: &syn::ExprArray) -> Option<Vec<(u16, String)>> {
    let responses: Vec<(u16, String)> = array
        .elems
        .iter()
        .filter_map(extract_typed_response)
        .collect();
    (!responses.is_empty()).then_some(responses)
}

fn extract_typed_response(elem: &syn::Expr) -> Option<(u16, String)> {
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
        let is_route_meta = check_route_by_meta(&attr.meta);
        if is_route_meta && let Some(info) = try_extract_from_meta(&attr.meta) {
            return Some(info);
        }
    }
    None
}

/// Translate a single `#[route(...)]` / `#[vespera::route(...)]` meta into
/// a [`RouteInfo`], handling all three meta shapes (List / NameValue /
/// Path).  Pulled out of [`extract_route_info`] so the per-shape branches
/// each get their own basic block in coverage instrumentation.
fn try_extract_from_meta(meta: &syn::Meta) -> Option<RouteInfo> {
    match meta {
        syn::Meta::List(meta_list) => {
            let route_args = meta_list.parse_args::<RouteArgs>().ok()?;
            Some(build_route_info_from_args(&route_args))
        }
        // `#[route = "patch"]` form.
        syn::Meta::NameValue(meta_nv) => {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = &meta_nv.value
            else {
                return None;
            };
            let method_str = lit_str.value().to_lowercase();
            if !is_http_method(&method_str) {
                return None;
            }
            Some(RouteInfo {
                method: method_str,
                path: None,
                success_status: None,
                error_status: None,
                typed_responses: None,
                tags: None,
                security: None,
                headers: Vec::new(),
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
            })
        }
        // `#[route]` bare form — defaults to GET.
        syn::Meta::Path(_) => Some(RouteInfo {
            method: "get".to_string(),
            path: None,
            success_status: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn parse_meta_from_attr(attr_str: &str) -> syn::Meta {
        // Parse attribute from string like "#[route()]" or "#[vespera::route(get)]"
        let full_code = format!("{attr_str} fn test() {{}}");
        let file: syn::File = syn::parse_str(&full_code).expect("Failed to parse with attribute");

        // Extract the first attribute from the function
        if let Some(syn::Item::Fn(fn_item)) = file.items.first()
            && let Some(attr) = fn_item.attrs.first()
        {
            return attr.meta.clone();
        }

        panic!("Failed to extract meta from attribute: {attr_str}");
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
            "Failed for attribute: {attr_str}, expected: {expected}"
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
    #[case("#[route = \"GET\"] fn test() {}", Some(("get".to_string(), None, None)))]
    // lowercase conversion
    // Non-string literal in NameValue format — value isn't a Lit::Str so
    // the `let ... else { return None; }` branch fires.
    #[case("#[route = 42] fn test() {}", None)]
    #[case("#[route = true] fn test() {}", None)]
    // Multiple attributes - should find route attribute
    #[case("#[derive(Debug)] #[route(get, path = \"/api\")] #[test] fn test() {}", Some(("get".to_string(), Some("/api".to_string()), None)))]
    // Multiple route attributes - first one wins
    #[case("#[route(get, path = \"/first\")] #[route(post, path = \"/second\")] fn test() {}", Some(("get".to_string(), Some("/first".to_string()), None)))]
    // Explicit tests for method.as_ref() and path.as_ref().map() coverage
    #[case("#[route(path = \"/test\")] fn test() {}", Some(("get".to_string(), Some("/test".to_string()), None)))] // method None, path Some
    #[case("#[route()] fn test() {}", Some(("get".to_string(), None, None)))] // method None, path None
    #[case("#[route(post)] fn test() {}", Some(("post".to_string(), None, None)))] // method Some, path None
    #[case("#[route(put, path = \"/test\")] fn test() {}", Some(("put".to_string(), Some("/test".to_string()), None)))] // method Some, path Some
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
                    "Expected Some but got None for code: {code}"
                );
                let route_info = result.unwrap();
                assert_eq!(
                    route_info.method, exp_method,
                    "Method mismatch for code: {code}"
                );
                assert_eq!(route_info.path, exp_path, "Path mismatch for code: {code}");
                assert_eq!(
                    route_info.error_status, exp_error_status,
                    "Error status mismatch for code: {code}"
                );
            }
            None => {
                assert!(
                    result.is_none(),
                    "Expected None but got Some({result:?}) for code: {code}"
                );
            }
        }
    }

    // Tests for extract_doc_comment function
    #[test]
    fn test_extract_doc_comment_single_line() {
        let code = r"
            /// This is a doc comment
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            let doc = extract_doc_comment(&fn_item.attrs);
            assert_eq!(doc, Some("This is a doc comment".to_string()));
        }
    }

    #[test]
    fn test_extract_doc_comment_multi_line() {
        let code = r"
            /// First line
            /// Second line
            /// Third line
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            let doc = extract_doc_comment(&fn_item.attrs);
            assert_eq!(doc, Some("First line\nSecond line\nThird line".to_string()));
        }
    }

    #[test]
    fn test_extract_doc_comment_empty() {
        let code = "fn test() {}";
        let file: syn::File = syn::parse_str(code).unwrap();
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            let doc = extract_doc_comment(&fn_item.attrs);
            assert_eq!(doc, None);
        }
    }

    #[test]
    fn test_extract_doc_comment_with_other_attrs() {
        let code = r"
            #[inline]
            /// Doc comment
            #[test]
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            let doc = extract_doc_comment(&fn_item.attrs);
            assert_eq!(doc, Some("Doc comment".to_string()));
        }
    }

    #[test]
    fn test_extract_doc_comment_no_leading_space() {
        let code = r"
            ///No leading space
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        if let Some(syn::Item::Fn(fn_item)) = file.items.first() {
            let doc = extract_doc_comment(&fn_item.attrs);
            assert_eq!(doc, Some("No leading space".to_string()));
        }
    }

    // Tests for tags and description in extract_route_info
    #[test]
    fn test_extract_route_info_with_tags() {
        let code = r#"#[route(get, tags = ["users", "admin"])] fn test() {}"#;
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(
            route_info.tags,
            Some(vec!["users".to_string(), "admin".to_string()])
        );
    }

    #[test]
    fn test_extract_route_info_with_single_tag() {
        let code = r#"#[route(get, tags = ["users"])] fn test() {}"#;
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(route_info.tags, Some(vec!["users".to_string()]));
    }

    #[test]
    fn test_extract_route_info_with_empty_tags() {
        let code = r"#[route(get, tags = [])] fn test() {}";
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(route_info.tags, None); // Empty array should return None
    }

    #[test]
    fn test_extract_route_info_with_description() {
        let code = r#"#[route(get, description = "Get all users")] fn test() {}"#;
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(route_info.description, Some("Get all users".to_string()));
    }

    #[test]
    fn test_extract_route_info_with_tags_and_description() {
        let code = r#"#[route(get, tags = ["users"], description = "Get users")] fn test() {}"#;
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(route_info.tags, Some(vec!["users".to_string()]));
        assert_eq!(route_info.description, Some("Get users".to_string()));
    }

    #[test]
    fn test_extract_route_info_all_options() {
        let code = r#"#[route(post, path = "/api/users", error_status = [400, 404], tags = ["users", "api"], description = "Create a new user")] fn test() {}"#;
        let attrs = parse_attrs_from_code(code);
        let result = extract_route_info(&attrs);
        assert!(result.is_some());
        let route_info = result.unwrap();
        assert_eq!(route_info.method, "post");
        assert_eq!(route_info.path, Some("/api/users".to_string()));
        assert_eq!(route_info.error_status, Some(vec![400, 404]));
        assert_eq!(
            route_info.tags,
            Some(vec!["users".to_string(), "api".to_string()])
        );
        assert_eq!(
            route_info.description,
            Some("Create a new user".to_string())
        );
    }
}
