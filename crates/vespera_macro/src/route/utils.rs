use crate::{args::RouteArgs, http::is_http_method, metadata::HeaderParam};

// Re-export the canonical `extract_doc_comment` implementation from the
// parser/schema/serde_attrs module so `crate::route::extract_doc_comment`
// (via `route/mod.rs`'s `pub use utils::*;`) continues to resolve for every
// production caller — `route_impl.rs` (route-attribute description fallback)
// and `collector.rs` (slow-path description fallback) — without holding a
// second byte-identical copy of the function.
pub use crate::parser::schema::extract_doc_comment;

/// Operation `(summary, description)` from the route arguments and the
/// handler's doc comment.
///
/// An explicit `description = ".."` wins and the doc comment is ignored.
/// Otherwise the doc comment is split the rustdoc way: its first paragraph
/// (lines up to the first blank line, joined with spaces so a wrapped
/// sentence stays whole) becomes the `summary` — unless `summary = ".."` is
/// given, in which case the WHOLE doc becomes the description — and the
/// paragraphs after it become the `description`.
pub fn doc_summary_and_description(
    summary: Option<String>,
    description: Option<String>,
    attrs: &[syn::Attribute],
) -> (Option<String>, Option<String>) {
    if description.is_some() {
        return (summary, description);
    }
    let Some(doc) = extract_doc_comment(attrs) else {
        return (summary, None);
    };
    if summary.is_some() {
        return (summary, Some(doc));
    }
    let (first, rest) = doc.split_once("\n\n").unwrap_or((doc.as_str(), ""));
    let summary = first.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let rest = rest.trim_start_matches('\n');
    (Some(summary), (!rest.is_empty()).then(|| rest.to_string()))
}

/// Whether a handler carries Rust's `#[deprecated]` attribute.
pub fn has_deprecated_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("deprecated"))
}

#[derive(Debug)]
pub struct RouteInfo {
    pub method: String,
    pub path: Option<String>,
    pub success_status: Option<u16>,
    pub error_status: Option<Vec<u16>>,
    pub typed_responses: Option<Vec<(u16, String)>>,
    pub tags: Option<Vec<String>>,
    pub security: Option<Vec<vespera_core::schema::SecurityRequirement>>,
    pub headers: Vec<HeaderParam>,
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub request_example: Option<serde_json::Value>,
    pub response_example: Option<serde_json::Value>,
    pub deprecated: bool,
    pub description: Option<String>,
}

/// Read an optional string-literal route argument into an owned `String`.
///
/// Deliberately written with `if let` instead of `Option::map(...)`: the
/// unwrap branch then belongs to a real source line rather than an internal
/// closure call site, which LLVM coverage reports as zero hits even when the
/// field is `Some`.  The lint suppression is scoped to exactly this helper —
/// every other line of [`build_route_info_from_args`] stays lint-checked.
#[allow(clippy::manual_map, clippy::option_if_let_else)]
fn lit_value(lit: Option<&syn::LitStr>) -> Option<String> {
    if let Some(lit) = lit {
        Some(lit.value())
    } else {
        None
    }
}

/// Convert a parsed [`RouteArgs`] into the simpler [`RouteInfo`] used by
/// the collector / OpenAPI emitter. The slow path runs the same strict
/// validators `#[route]` expansion runs (`route_impl`): an argument the
/// expansion rejects makes the route `None`, so the route is not documented.
/// Documenting it with that argument dropped would write a spec weaker than
/// the source (the expansion's compile error is the diagnostic).
fn build_route_info_from_args(route_args: &RouteArgs) -> Option<RouteInfo> {
    let method = route_args
        .method
        .as_ref()
        .map_or_else(|| "get".to_string(), syn::Ident::to_string);
    let path = lit_value(route_args.path.as_ref());

    let error_status = match route_args.error_status.as_ref() {
        Some(arr) => crate::route_impl::extract_error_status_codes(arr).ok()?,
        None => None,
    };
    let tags = match route_args.tags.as_ref() {
        Some(arr) => crate::route_impl::extract_tag_strings(arr).ok()?,
        None => None,
    };
    let typed_responses = match route_args.responses.as_ref() {
        Some(arr) => crate::route_impl::extract_typed_responses(arr).ok()?,
        None => None,
    };
    let security = match route_args.security.as_ref() {
        Some(arr) => {
            Some(crate::security::parse_security_requirements(arr, "#[route] `security`").ok()?)
        }
        None => None,
    };
    let headers = route_args.headers.clone().unwrap_or_default();

    let description = lit_value(route_args.description.as_ref());
    let operation_id = lit_value(route_args.operation_id.as_ref());
    let summary = lit_value(route_args.summary.as_ref());

    let request_example = route_args
        .request_example
        .as_ref()
        .map(parse_example_string);
    let response_example = route_args
        .response_example
        .as_ref()
        .map(parse_example_string);

    Some(RouteInfo {
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
    })
}

fn parse_example_string(lit: &syn::LitStr) -> serde_json::Value {
    let value = lit.value();
    serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
}

/// The `responses = [(404, errors::NotFound)]` type as written
/// (`errors::NotFound`): the module qualifier lets the OpenAPI generator tell
/// same-named renamed types apart. The segment idents only, so generic
/// arguments are dropped; the `#[route]` argument parser (`args.rs`,
/// `responses`) rejects a path that has them, so only argument-free paths reach
/// this.
pub fn response_type_path(path: &syn::Path) -> Option<String> {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    (!segments.is_empty()).then(|| segments.join("::"))
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
            build_route_info_from_args(&route_args)
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

    #[rstest::rstest]
    #[case::wrapped_summary(None, None, &[" Get a user", " by id.", "", "", " More."], (Some("Get a user by id."), Some("More.")))]
    #[case::first_paragraph_is_summary(None, None, &[" Get a user.", "", " Looks it up", " by id."], (Some("Get a user."), Some("Looks it up\nby id.")))]
    #[case::single_line(None, None, &[" Get a user."], (Some("Get a user."), None))]
    #[case::explicit_summary_keeps_whole_doc(Some("S"), None, &[" A", " B"], (Some("S"), Some("A\nB")))]
    #[case::explicit_description_ignores_doc(None, Some("D"), &[" A"], (None, Some("D")))]
    #[case::no_doc(Some("S"), None, &[], (Some("S"), None))]
    fn doc_comment_splits_into_summary_and_description(
        #[case] summary: Option<&str>,
        #[case] description: Option<&str>,
        #[case] doc: &[&str],
        #[case] expected: (Option<&str>, Option<&str>),
    ) {
        let attrs: Vec<syn::Attribute> = doc
            .iter()
            .map(|line| syn::parse_quote!(#[doc = #line]))
            .collect();
        let (s, d) = doc_summary_and_description(
            summary.map(str::to_string),
            description.map(str::to_string),
            &attrs,
        );
        assert_eq!((s.as_deref(), d.as_deref()), expected);
    }

    #[test]
    fn deprecated_attribute_is_detected() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[deprecated(note = "use v2")]
            async fn old() {}
        };
        assert!(has_deprecated_attr(&item.attrs));
        let item: syn::ItemFn = syn::parse_quote!(
            async fn new() {}
        );
        assert!(!has_deprecated_attr(&item.attrs));
    }

    // Tests for extract_doc_comment function
    #[test]
    fn test_extract_doc_comment_single_line() {
        let code = r"
            /// This is a doc comment
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        let Some(syn::Item::Fn(fn_item)) = file.items.first() else {
            panic!("expected a fn item");
        };
        let doc = extract_doc_comment(&fn_item.attrs);
        assert_eq!(doc, Some("This is a doc comment".to_string()));
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
        let Some(syn::Item::Fn(fn_item)) = file.items.first() else {
            panic!("expected a fn item");
        };
        let doc = extract_doc_comment(&fn_item.attrs);
        assert_eq!(doc, Some("First line\nSecond line\nThird line".to_string()));
    }

    #[test]
    fn test_extract_doc_comment_empty() {
        let code = "fn test() {}";
        let file: syn::File = syn::parse_str(code).unwrap();
        let Some(syn::Item::Fn(fn_item)) = file.items.first() else {
            panic!("expected a fn item");
        };
        let doc = extract_doc_comment(&fn_item.attrs);
        assert_eq!(doc, None);
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
        let Some(syn::Item::Fn(fn_item)) = file.items.first() else {
            panic!("expected a fn item");
        };
        let doc = extract_doc_comment(&fn_item.attrs);
        assert_eq!(doc, Some("Doc comment".to_string()));
    }

    #[test]
    fn test_extract_doc_comment_no_leading_space() {
        let code = r"
            ///No leading space
            fn test() {}
        ";
        let file: syn::File = syn::parse_str(code).unwrap();
        let Some(syn::Item::Fn(fn_item)) = file.items.first() else {
            panic!("expected a fn item");
        };
        let doc = extract_doc_comment(&fn_item.attrs);
        assert_eq!(doc, Some("No leading space".to_string()));
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

    #[rstest]
    #[case::json_object(r#"{"id":7}"#, serde_json::json!({"id": 7}))]
    #[case::plain_text("not json", serde_json::Value::String("not json".to_string()))]
    fn parse_example_string_preserves_json_or_falls_back_to_text(
        #[case] source: &str,
        #[case] expected: serde_json::Value,
    ) {
        let literal = syn::LitStr::new(source, proc_macro2::Span::call_site());

        assert_eq!(parse_example_string(&literal), expected);
    }

    /// The slow path (a file parsed without expansion) runs the same strict
    /// validators as `#[route]` expansion: a route with an argument that
    /// expansion rejects is not documented at all.
    #[rstest::rstest]
    #[case::tag_not_a_string(r#"#[route(get, tags = ["users", admin])] fn t() {}"#)]
    #[case::error_status_out_of_range(r"#[route(get, error_status = [200])] fn t() {}")]
    #[case::error_status_not_an_integer(r"#[route(get, error_status = [BAD])] fn t() {}")]
    #[case::response_not_a_tuple(r"#[route(get, responses = [NotFound])] fn t() {}")]
    #[case::response_status_repeated(r"#[route(get, responses = [(404, A), (404, B)])] fn t() {}")]
    #[case::security_not_a_scheme(r"#[route(get, security = [42])] fn t() {}")]
    fn slow_path_skips_a_route_the_expansion_rejects(#[case] code: &str) {
        assert!(
            extract_route_info(&parse_attrs_from_code(code)).is_none(),
            "{code}"
        );
    }

    #[rstest::rstest]
    #[case::typed_response(
        r"#[route(get, responses = [(404, errors::NotFound)])] fn t() {}",
        Some(vec![(404, "errors::NotFound".to_string())])
    )]
    fn slow_path_reads_typed_responses(
        #[case] code: &str,
        #[case] expected: Option<Vec<(u16, String)>>,
    ) {
        let route_info = extract_route_info(&parse_attrs_from_code(code)).expect("route attribute");
        assert_eq!(route_info.typed_responses, expected);
    }

    #[rstest::rstest]
    #[case::scoped(
        r#"#[route(get, security = [("oauth2", ["read:users"]), "apiKey"])] fn t() {}"#,
        Some(r#"[{"oauth2":["read:users"]},{"apiKey":[]}]"#)
    )]
    fn slow_path_reads_scoped_security(#[case] code: &str, #[case] expected: Option<&str>) {
        let route_info = extract_route_info(&parse_attrs_from_code(code)).expect("route attribute");
        assert_eq!(
            route_info
                .security
                .map(|security| serde_json::to_string(&security).unwrap())
                .as_deref(),
            expected
        );
    }
}
