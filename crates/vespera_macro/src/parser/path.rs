/// Extract path parameters from a path string.
///
/// An axum catch-all segment `{*rest}` names the parameter `rest`.
pub fn extract_path_parameters(path: &str) -> Vec<String> {
    let mut params = Vec::new();

    // Iterate the split lazily — no intermediate `Vec<&str>` allocation.
    for segment in path.split('/') {
        if let Some(param) = segment
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
        {
            params.push(param.strip_prefix('*').unwrap_or(param).to_string());
        } else if let Some(param) = segment.strip_prefix(':') {
            params.push(param.to_string());
        }
    }

    params
}

/// The `OpenAPI` path-template key for an axum route path.
///
/// axum spells a catch-all segment `{*rest}`; `OpenAPI` path templates have no
/// wildcard syntax, so the segment is documented as the plain parameter
/// `{rest}` (the router keeps registering `{*rest}`).
pub fn openapi_path_template(path: &str) -> String {
    path.replace("{*", "{")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("/test", vec![])]
    #[case("/test/{id}", vec!["id"])]
    #[case("/test/{id}/test/{test_id}", vec!["id", "test_id"])]
    #[case("/test/:id/test/:test_id", vec!["id", "test_id"])]
    #[case("/files/{*rest}", vec!["rest"])]
    #[case("/a/{id}/{*path}", vec!["id", "path"])]
    fn test_extract_path_parameters(#[case] path: &str, #[case] expected: Vec<&str>) {
        assert_eq!(extract_path_parameters(path), expected);
    }

    #[rstest]
    #[case("/files/{*rest}", "/files/{rest}")]
    #[case("/a/{id}/{*path}", "/a/{id}/{path}")]
    #[case("/plain/{id}", "/plain/{id}")]
    fn test_openapi_path_template(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(openapi_path_template(path), expected);
    }
}
