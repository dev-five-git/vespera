//! Unit tests for a test-gated `mod` whose cfg-disjoint twin is an inline
//! module (round-34 R34-MR2): the twin's own children load in every non-test
//! build, even though the gated declaration of the same name does not.

use super::tests::crate_with;
use super::*;

/// `#[cfg(test)] mod a;` beside `#[cfg(not(test))] mod a { mod b; }`: the
/// inline twin loads `a/b.rs` in every non-test build, so `a/b.rs` is
/// production (`a.rs`, loaded only by the gated declaration, is not).
#[test]
fn file_loaded_by_an_ungated_inline_twin_chain_is_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\nmod a;\n#[cfg(not(test))]\nmod a {\n    mod b;\n}\n",
        ),
        ("a.rs", "pub struct Gated {}"),
        ("a/b.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("a/b.rs")));
    assert!(scope.file_test_only(&src.join("a.rs")));
}

/// The same gated `mod a;` with an inline twin that does not name `b`: `a/b.rs`
/// is loaded by no build but a `cfg(test)` one, so it stays test-only.
#[test]
fn file_under_a_gated_module_with_an_unrelated_inline_twin_stays_test_only() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\nmod a;\n#[cfg(not(test))]\nmod a {\n    mod c;\n}\n",
        ),
        ("a/b.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("a/b.rs")));
}
