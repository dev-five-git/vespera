/// Extract path parameters from a path string.
///
/// An axum catch-all segment `{*rest}` names the parameter `rest`; a
/// parameter after a static prefix (`file-{name}`) is found too, and escaped
/// literal braces (`{{x}}`) are not parameters. A malformed segment (already
/// rejected by [`normalize_route_path`]) contributes nothing.
pub fn extract_path_parameters(path: &str) -> Vec<String> {
    let mut params = Vec::new();

    // Iterate the split lazily — no intermediate `Vec<&str>` allocation.
    for segment in path.split('/') {
        if let Some(param) = segment.strip_prefix(':') {
            params.push(param.to_string());
        } else {
            let found = brace_params(path, segment).unwrap_or_default();
            params.extend(found.into_iter().map(|(name, _)| name));
        }
    }

    params
}

/// The `OpenAPI` path-template key for an axum route path.
///
/// axum spells a catch-all segment `{*rest}`; `OpenAPI` path templates have no
/// wildcard syntax, so the segment is documented as the plain parameter
/// `{rest}` (the router keeps registering `{*rest}`). axum's escaped literal
/// braces `{{` / `}}` cannot appear unescaped in a template (they would read
/// as a parameter), so they are percent-encoded (`%7B` / `%7D`).
pub fn openapi_path_template(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if !in_param && chars.next_if_eq(&'{').is_some() => out.push_str("%7B"),
            '}' if !in_param && chars.next_if_eq(&'}').is_some() => out.push_str("%7D"),
            '{' => {
                in_param = true;
                out.push('{');
                chars.next_if_eq(&'*');
            }
            '}' => {
                in_param = false;
                out.push('}');
            }
            c => out.push(c),
        }
    }
    out
}

/// Normalize a `#[route(path = "...")]` literal to axum 0.8 syntax and
/// reject malformed templates with an error spanned on the literal.
///
/// axum 0.7 spellings are accepted and rewritten per segment (axum 0.8
/// panics on them when the router is built): `:name` -> `{name}`, `*name`
/// -> `{*name}`. Mixed styles in one path are fine. Errors: unbalanced or
/// nested braces, an empty parameter (`{}`, `:`, `*`, `{*}`), a duplicate
/// parameter name, a catch-all that is not the whole last segment, and text
/// after a parameter in its segment (`{name}.png`; a prefix — `file-{name}`
/// — is fine).
/// `{{` / `}}` stay literal braces (axum's escape).
pub fn normalize_route_path(lit: &syn::LitStr) -> syn::Result<syn::LitStr> {
    let path = lit.value();
    let normalized = normalize_path_str(&path).map_err(|msg| syn::Error::new(lit.span(), msg))?;
    if normalized == path {
        return Ok(lit.clone());
    }
    Ok(syn::LitStr::new(&normalized, lit.span()))
}

fn normalize_path_str(path: &str) -> Result<String, String> {
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    let mut names: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        let (normalized, params) = if let Some(name) = segment.strip_prefix(':') {
            check_v07_name(path, ':', name)?;
            (format!("{{{name}}}"), vec![(name.to_string(), false)])
        } else if let Some(name) = segment.strip_prefix('*') {
            check_v07_name(path, '*', name)?;
            (format!("{{*{name}}}"), vec![(name.to_string(), true)])
        } else {
            ((*segment).to_string(), brace_params(path, segment)?)
        };
        for (name, catch_all) in params {
            if catch_all && (i != last || normalized != format!("{{*{name}}}")) {
                return Err(format!(
                    "route path `{path}`: the catch-all `{{*{name}}}` must be the whole last segment"
                ));
            }
            if names.contains(&name) {
                return Err(format!(
                    "route path `{path}`: duplicate path parameter `{name}`"
                ));
            }
            names.push(name);
        }
        out.push(normalized);
    }
    Ok(out.join("/"))
}

/// The name after an axum 0.7 `:` / `*` segment prefix.
fn check_v07_name(path: &str, prefix: char, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!(
            "route path `{path}`: `{prefix}` must be followed by a parameter name (`{prefix}id`)"
        ));
    }
    if name.contains(['{', '}', ':', '*']) {
        return Err(format!(
            "route path `{path}`: invalid parameter name `{name}` after `{prefix}`"
        ));
    }
    Ok(())
}

/// The `{name}` / `{*name}` parameters of one segment, `(name, catch_all)`.
///
/// axum's router (matchit 0.8) allows a static prefix before a parameter
/// (`file-{name}`) but nothing after it — `{name}.png` / `{a}{b}` panic when
/// the router is built — so text after a parameter is an error here.
fn brace_params(path: &str, segment: &str) -> Result<Vec<(String, bool)>, String> {
    let mut params = Vec::new();
    let mut open: Option<String> = None;
    let mut closed: Option<String> = None;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(raw) = &closed {
            return Err(format!(
                "route path `{path}`: axum's router cannot match text after the path parameter \
                 `{{{raw}}}` in segment `{segment}`: a parameter must end its segment (a static \
                 prefix such as `file-{{name}}` is fine)"
            ));
        }
        match (c, open.as_mut()) {
            ('{', None) if chars.peek() == Some(&'{') => {
                chars.next();
            }
            ('}', None) if chars.peek() == Some(&'}') => {
                chars.next();
            }
            ('{', None) => open = Some(String::new()),
            ('{', Some(_)) => {
                return Err(format!(
                    "route path `{path}`: nested `{{` in segment `{segment}`"
                ));
            }
            ('}', None) => {
                return Err(format!(
                    "route path `{path}`: unbalanced `}}` in segment `{segment}`"
                ));
            }
            ('}', Some(_)) => {
                let raw = open.take().unwrap_or_default();
                let (name, catch_all) = raw
                    .strip_prefix('*')
                    .map_or((raw.as_str(), false), |name| (name, true));
                if name.is_empty() {
                    return Err(format!(
                        "route path `{path}`: empty path parameter `{{{raw}}}`"
                    ));
                }
                params.push((name.to_string(), catch_all));
                closed = Some(raw);
            }
            (c, Some(name)) => name.push(c),
            (_, None) => {}
        }
    }
    if open.is_some() {
        return Err(format!(
            "route path `{path}`: unclosed `{{` in segment `{segment}`"
        ));
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("/users", "/users")]
    #[case("/users/{id}", "/users/{id}")]
    #[case("/users/:id", "/users/{id}")]
    #[case("/users/:id/posts/:post_id", "/users/{id}/posts/{post_id}")]
    #[case("/files/*rest", "/files/{*rest}")]
    #[case("/a/:id/{b}/*rest", "/a/{id}/{b}/{*rest}")]
    #[case("/files/{*rest}", "/files/{*rest}")]
    #[case("/img/file-{name}", "/img/file-{name}")]
    #[case("/img/{{{name}", "/img/{{{name}")]
    #[case("/lit/{{x}}", "/lit/{{x}}")]
    #[case("/time/a:b", "/time/a:b")]
    #[case("", "")]
    #[case("/", "/")]
    fn normalizes_route_paths(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(normalize_path_str(path).as_deref(), Ok(expected));
    }

    #[rstest]
    #[case::unclosed("/{id", "unclosed `{` in segment `{id`")]
    #[case::unbalanced_close("/id}", "unbalanced `}` in segment `id}`")]
    #[case::nested("/{a{b}}", "nested `{` in segment `{a{b}}`")]
    #[case::empty_braces("/{}", "empty path parameter `{}`")]
    #[case::empty_catch_all("/{*}", "empty path parameter `{*}`")]
    #[case::empty_colon("/a/:", "`:` must be followed by a parameter name (`:id`)")]
    #[case::empty_star("/a/*", "`*` must be followed by a parameter name (`*id`)")]
    #[case::bad_colon_name("/a/:{id}", "invalid parameter name `{id}` after `:`")]
    #[case::duplicate("/a/:id/{id}", "duplicate path parameter `id`")]
    #[case::catch_all_not_last(
        "/a/*rest/b",
        "the catch-all `{*rest}` must be the whole last segment"
    )]
    #[case::catch_all_partial(
        "/a/x{*rest}",
        "the catch-all `{*rest}` must be the whole last segment"
    )]
    #[case::static_suffix(
        "/img/{name}.png",
        "axum's router cannot match text after the path parameter `{name}` in segment `{name}.png`: a parameter must end its segment (a static prefix such as `file-{name}` is fine)"
    )]
    #[case::two_params_one_segment(
        "/{a}{b}",
        "axum's router cannot match text after the path parameter `{a}` in segment `{a}{b}`: a parameter must end its segment (a static prefix such as `file-{name}` is fine)"
    )]
    #[case::escaped_brace_suffix(
        "/{a}}}",
        "axum's router cannot match text after the path parameter `{a}` in segment `{a}}}`: a parameter must end its segment (a static prefix such as `file-{name}` is fine)"
    )]
    fn rejects_malformed_route_paths(#[case] path: &str, #[case] message: &str) {
        assert_eq!(
            normalize_path_str(path),
            Err(format!("route path `{path}`: {message}"))
        );
    }

    #[test]
    fn normalized_literal_keeps_the_span_and_reuses_unchanged_literals() {
        let lit: syn::LitStr = syn::parse_quote!("/users/:id");
        assert_eq!(normalize_route_path(&lit).unwrap().value(), "/users/{id}");
        let lit: syn::LitStr = syn::parse_quote!("/users/{id}");
        assert_eq!(normalize_route_path(&lit).unwrap(), lit);
        let lit: syn::LitStr = syn::parse_quote!("/users/{id");
        assert!(normalize_route_path(&lit).is_err());
    }

    #[rstest]
    #[case("/test", vec![])]
    #[case("/test/{id}", vec!["id"])]
    #[case("/test/{id}/test/{test_id}", vec!["id", "test_id"])]
    #[case("/test/:id/test/:test_id", vec!["id", "test_id"])]
    #[case("/files/{*rest}", vec!["rest"])]
    #[case("/a/{id}/{*path}", vec!["id", "path"])]
    #[case("/img/file-{name}", vec!["name"])]
    #[case("/lit/{{x}}", vec![])]
    #[case("/lit/{{x}}/{id}", vec!["id"])]
    #[case("/a/{{{id}", vec!["id"])]
    #[case("/bad/{id", vec![])]
    fn test_extract_path_parameters(#[case] path: &str, #[case] expected: Vec<&str>) {
        assert_eq!(extract_path_parameters(path), expected);
    }

    #[rstest]
    #[case("/files/{*rest}", "/files/{rest}")]
    #[case("/a/{id}/{*path}", "/a/{id}/{path}")]
    #[case("/plain/{id}", "/plain/{id}")]
    #[case("/img/file-{name}", "/img/file-{name}")]
    // `{{` / `}}` are literal braces, which an OpenAPI template cannot spell
    // unescaped (it would read `{x}` as a parameter): percent-encoded.
    #[case("/lit/{{x}}", "/lit/%7Bx%7D")]
    #[case("/a/{{{id}", "/a/%7B{id}")]
    #[case("/a/}}{id}", "/a/%7D{id}")]
    fn test_openapi_path_template(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(openapi_path_template(path), expected);
    }
}
