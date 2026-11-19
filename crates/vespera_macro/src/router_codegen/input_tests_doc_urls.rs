use super::*;
use rstest::rstest;

/// A documentation route is requested under the path a client resolves to, so
/// dot segments (also percent-encoded) and backslashes are compile errors.
#[rstest]
#[case::dot_dot("/api/../docs")]
#[case::dot("/./redoc")]
#[case::encoded_dot_dot("/api/%2E%2e/docs")]
#[case::backslash("/api\\docs")]
fn test_validate_doc_urls_rejects_resolved_paths(#[case] url: &str) {
    let lit = LitStr::new(url, Span::call_site());
    for key in ["docs_url", "redoc_url", "spec_url"] {
        let error = validate_doc_urls(&[(key, Some(&lit))]).expect_err(url);
        assert!(
            error.to_string().contains("must not contain `.` or `..`"),
            "{url}: {error}"
        );
    }
}

#[rstest]
#[case::plain("/docs")]
#[case::nested("/api/v1/docs")]
#[case::dotted_name("/api/v1.2/docs")]
fn test_validate_doc_urls_accepts_plain_paths(#[case] url: &str) {
    let lit = LitStr::new(url, Span::call_site());
    assert!(
        validate_doc_urls(&[("docs_url", Some(&lit))]).is_ok(),
        "{url}"
    );
}
