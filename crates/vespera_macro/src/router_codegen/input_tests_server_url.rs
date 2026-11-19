use super::*;
use rstest::rstest;

/// A relative server URL is a path on the document's host, so it carries no
/// whitespace, query or fragment (the absolute form already rejects
/// whitespace; the relative form also rejects `?` and `#`).
#[rstest]
#[case::space_in_path("/api v1")]
#[case::tab_in_path("/api\tv1")]
#[case::query("/api?x=1")]
#[case::fragment("/api#top")]
fn test_validate_server_url_rejects_spaced_or_query_relative(#[case] url: &str) {
    let lit = LitStr::new(url, Span::call_site());
    assert!(validate_server_url(&lit).is_err(), "{url}");
    assert!(!is_server_url(url));
}

#[rstest]
#[case::path("/api/v1")]
#[case::root("/")]
fn test_validate_server_url_accepts_plain_relative(#[case] url: &str) {
    let lit = LitStr::new(url, Span::call_site());
    assert!(validate_server_url(&lit).is_ok(), "{url}");
    assert!(is_server_url(url));
}
