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

/// A test-only inline module with a child `mod` beside an ungated `mod a;`
/// twin (round-39 MR1-M1, the reversed twin): `a/b.rs` is loaded only by
/// `cfg(test)` builds, while `a.rs` is the production module.
#[test]
fn gated_inline_parent_keeps_its_child_test_only_beside_an_ungated_twin() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)] mod a { mod b; }
#[cfg(not(test))] mod a;",
        ),
        ("a.rs", ""),
        ("a/b.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("a/b.rs")));
    assert!(!scope.file_test_only(&src.join("a.rs")));
}

/// A verbatim expression has no attributes to read (the wildcard arm of
/// `expr_attrs`), and every other statement expression reads its own.
#[test]
fn expr_attrs_of_a_verbatim_expression_is_empty() {
    let expr = syn::Expr::Verbatim(proc_macro2::TokenStream::default());
    assert_eq!(expr_attrs(&expr), &[]);
}

/// Identical-signature `cfg(test)` / `cfg(not(test))` twins of one route
/// (round-42 MR42-1-M1): the storage holds only the twin this build compiles,
/// so the stub is test-only in the `--test` build and the production fn is
/// not, in the other build. Both builds are pinned through the compiler args.
#[test]
fn identical_signature_cfg_twins_follow_the_build_that_stored_them() {
    let krate = crate_with(&[(
        "twin_health.rs",
        "#[cfg(test)]\n#[route(get, path = \"/stub\")]\npub async fn health() -> u8 { 0 }\n\
         #[cfg(not(test))]\n#[route(get)]\npub async fn health() -> u8 { 0 }\n",
    )]);
    let defs = fn_definitions(&krate.path().join("src").join("twin_health.rs"));
    assert_eq!(defs.len(), 2);
    assert!(defs[0].test_only && !defs[1].test_only);
    let signature = defs[0].signature.clone();
    assert_eq!(signature, defs[1].signature);
    {
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--test",
        );
        assert!(route_test_only(&defs, "health", "get", &signature));
    }
    {
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\ndemo",
        );
        assert!(!route_test_only(&defs, "health", "get", &signature));
    }
}
