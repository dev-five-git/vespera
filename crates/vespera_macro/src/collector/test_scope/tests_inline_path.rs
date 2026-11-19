//! Unit tests for the inline `#[path]` modules of `collector::test_scope`: an
//! inline `#[path = "d"] mod h { .. }` loads its children from `d/`, as rustc
//! does, not from `h/`.

use super::tests::crate_with;
use super::*;

/// A test-gated child of an inline `#[path = "fixtures"]` module is loaded
/// from `fixtures/x.rs`, so that file is test-only. `helpers/x.rs`, the
/// directory the module's name suggests, is compiled by no build.
#[test]
fn test_gated_child_of_an_inline_path_module_is_test_only() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[path = \"fixtures\"]\nmod helpers {\n    #[cfg(test)]\n    mod x;\n}\n",
        ),
        ("fixtures/x.rs", "pub struct Fixture {}"),
        ("helpers/x.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("fixtures/x.rs")));
    assert!(!scope.file_test_only(&src.join("helpers/x.rs")));
}

/// An ungated child of an inline `#[path]` module is loaded in every build,
/// so its file stays production.
#[test]
fn ungated_child_of_an_inline_path_module_is_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[path = \"fixtures\"]\nmod helpers {\n    mod x;\n}\n",
        ),
        ("fixtures/x.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("fixtures/x.rs")));
}

/// A test-gated inline `#[path]` module gates the children it loads, even the
/// ungated ones.
#[test]
fn ungated_child_of_a_gated_inline_path_module_is_test_only() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"fixtures\"]\nmod helpers {\n    mod x;\n}\n",
        ),
        ("fixtures/x.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("fixtures/x.rs")));
}

/// An inline `#[path]` module nested in another inline module loads its
/// children under the enclosing block's directory: `outer/fixtures/x.rs`, not
/// `outer/helpers/x.rs`.
#[test]
fn gated_inline_path_module_nests_under_its_enclosing_block() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "mod outer {\n    #[cfg(test)]\n    #[path = \"fixtures\"]\n    mod helpers {\n        mod x;\n    }\n}\n",
        ),
        ("outer/fixtures/x.rs", "pub struct Fixture {}"),
        ("outer/helpers/x.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("outer/fixtures/x.rs")));
    assert!(!scope.file_test_only(&src.join("outer/helpers/x.rs")));
}
