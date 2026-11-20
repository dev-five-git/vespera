//! Unit tests for the `cfg_attr`-wrapped `cfg`s of `collector::test_scope`:
//! `#[cfg_attr(all(), cfg(test))]` is a `cfg(test)` gate (round-32
//! macrorest-1), `#[cfg_attr(feature = "x", cfg(test))]` is not.

use rstest::rstest;

use super::tests::crate_with;
use super::*;

#[rstest]
#[case::statically_true_cfg_attr_test("#[cfg_attr(all(), cfg(test))]", true)]
#[case::direct_cfg_test("#[cfg(test)]", true)]
#[case::feature_gated_cfg_attr_test("#[cfg_attr(feature = \"x\", cfg(test))]", false)]
#[case::statically_false_cfg_attr_test("#[cfg_attr(any(), cfg(test))]", false)]
#[case::test_or_unix_is_not_test_only("#[cfg_attr(all(), cfg(any(test, unix)))]", false)]
#[case::cfg_attr_derive_is_not_a_gate("#[cfg_attr(all(), derive(Debug))]", false)]
fn attrs_test_only_follows_cfg_attr(#[case] attr: &str, #[case] expected: bool) {
    let attrs = syn::parse::Parser::parse_str(syn::Attribute::parse_outer, attr).unwrap();
    assert_eq!(attrs_test_only(&attrs), expected);
}

/// A `mod` declared under a statically true `cfg_attr(all(), cfg(test))` is
/// loaded only by `cfg(test)` builds, so its file is test-only; a feature
/// `cfg_attr` keeps the file production.
#[test]
fn cfg_attr_gated_module_is_test_only_only_when_statically_true() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg_attr(all(), cfg(test))]\nmod fixtures;\n#[cfg_attr(feature = \"x\", cfg(test))]\nmod kept;\n",
        ),
        ("fixtures.rs", "pub struct Fixture {}"),
        ("kept.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("fixtures.rs")));
    assert!(!scope.file_test_only(&src.join("kept.rs")));
}
