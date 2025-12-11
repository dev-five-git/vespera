/// Extract path parameters from a path string
pub fn extract_path_parameters(path: &str) -> Vec<String> {
    let mut params = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();

    for segment in segments {
        if segment.starts_with('{') && segment.ends_with('}') {
            let param = segment.trim_start_matches('{').trim_end_matches('}');
            params.push(param.to_string());
        } else if segment.starts_with(':') {
            let param = segment.trim_start_matches(':');
            params.push(param.to_string());
        }
    }

    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("/test", vec![])]
    #[case("/test/{id}", vec!["id"])]
    #[case("/test/{id}/test/{test_id}", vec!["id", "test_id"])]
    #[case("/test/:id/test/:test_id", vec!["id", "test_id"])]
    fn test_extract_path_parameters(#[case] path: &str, #[case] expected: Vec<&str>) {
        assert_eq!(extract_path_parameters(path), expected);
    }
}
