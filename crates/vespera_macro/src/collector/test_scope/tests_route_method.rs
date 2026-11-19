//! Unit tests for the `#[route]` method [`super::fn_definitions`] records: the
//! `#[route = "post"]` name-value form carries its method like `#[route(post)]`.

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
