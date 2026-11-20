//! Unit tests for the `#[route]` method [`super::fn_definitions`] records: the
//! `#[route = "post"]` name-value form carries its method like `#[route(post)]`.

use rstest::rstest;

use super::tests::crate_with;
use super::*;

/// A `#[route = "post"]` production fn beside a `cfg(test)` `#[route(post)]`
/// twin of the same name is recorded as `post` (not `get`), so the production
/// POST stays documented; the lower-case rule applies to the name-value form
/// too, and a bare `#[route]` or an unknown name-value method keeps `get`.
#[test]
fn name_value_route_method_is_recorded_like_the_list_form() {
    let krate = crate_with(&[(
        "named.rs",
        "#[cfg(test)]\n#[route(post)]\npub async fn create() {}\n#[route = \"post\"]\npub async fn create() {}\n#[route = \"PATCH\"]\npub async fn patch() {}\n#[route]\npub async fn bare() {}\n#[route = \"nope\"]\npub async fn odd() {}\n",
    )]);
    let defs = fn_definitions(&krate.path().join("src").join("named.rs"));
    let methods: Vec<&str> = defs.iter().map(|def| def.method.as_str()).collect();
    assert_eq!(methods, ["post", "post", "patch", "get", "get"]);
    let signature = defs[1].signature.clone();
    assert!(!route_test_only(&defs, "create", "post", &signature));
}

/// `trace` is a `#[route]` method like the others (round-32 macrorest-0 R32-M1):
/// the list and name-value forms both record `trace`, so a `cfg(test)` TRACE
/// twin is classified as test-only against its production twin.
#[rstest]
#[case::list_form("#[route(trace)]\npub async fn probe() {}\n", "trace")]
#[case::list_form_upper("#[route(TRACE)]\npub async fn probe() {}\n", "trace")]
#[case::name_value_form("#[route = \"trace\"]\npub async fn probe() {}\n", "trace")]
#[case::name_value_upper("#[route = \"TRACE\"]\npub async fn probe() {}\n", "trace")]
#[case::unknown_name_value("#[route = \"connect\"]\npub async fn probe() {}\n", "get")]
fn trace_route_method_is_recorded(#[case] source: &str, #[case] expected: &str) {
    let krate = crate_with(&[("traced.rs", source)]);
    let defs = fn_definitions(&krate.path().join("src").join("traced.rs"));
    let methods: Vec<&str> = defs.iter().map(|def| def.method.as_str()).collect();
    assert_eq!(methods, [expected]);
}

/// A `cfg(test)` `#[route(trace)]` twin is test-only beside a production
/// `#[route(trace)]` of the same name (the TRACE twin is not misread as `get`).
#[test]
fn cfg_test_trace_twin_is_test_only() {
    let source = "#[cfg(test)]\n#[route(trace)]\npub async fn probe() {}\n#[route(trace)]\npub async fn probe() {}\n";
    let krate = crate_with(&[("traced.rs", source)]);
    let defs = fn_definitions(&krate.path().join("src").join("traced.rs"));
    assert_eq!(defs.len(), 2);
    assert!(defs[0].test_only);
    assert!(!defs[1].test_only);
}
