//! Unit tests for a test-gated `mod` whose cfg-disjoint twin is an inline
//! module (round-34 R34-MR2): the twin's own children load in every non-test
//! build, even though the gated declaration of the same name does not.

use super::tests::crate_with;
use super::*;
use crate::route_impl::StoredRouteInfo;

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

/// The `cfg(test)` / `cfg(not(test))` twins of one route `health`, as the
/// stub's crate file declares them.
const TWIN_HEALTH: &str = "pub const LIMIT: u8 = 1;
\n     #[cfg(test)]\n#[route(get, path = \"/stub\")]\npub async fn health() -> u8 { 0 }\n\
     #[cfg(not(test))]\n#[route(get)]\npub async fn health() -> u8 { 0 }\n";

/// Both compiler-argv forms the macro can see: the `--test` harness build and
/// a plain build (rust-analyzer's argv has no `--test` either).
const ARGV_FORMS: [&str; 2] = ["rustc\n--test", "rustc\n--crate-name\ndemo"];

/// The stored entry of the `health` twin with the given own `cfg` verdict.
fn stored_health(
    file: &str,
    signature: &str,
    own_test_only: bool,
    path: Option<&str>,
) -> StoredRouteInfo {
    StoredRouteInfo {
        fn_name: "health".to_string(),
        method: Some("get".to_string()),
        custom_path: path.map(str::to_string),
        fn_sig_str: signature.to_string(),
        file_path: Some(file.to_string()),
        own_test_only,
        ..StoredRouteInfo::default()
    }
}

/// Identical-signature `cfg(test)` / `cfg(not(test))` twins of one route
/// (round-42 MR42-1-M1, round-45 MR1-M1): the storage holds only the twin this
/// build compiles, and its own `cfg` says which one it is. No compiler argument
/// is read, so the verdict is the same under every argv form.
#[test]
fn identical_signature_cfg_twins_follow_the_build_that_stored_them() {
    let krate = crate_with(&[("twin_health.rs", TWIN_HEALTH)]);
    let defs = fn_definitions(&krate.path().join("src").join("twin_health.rs"));
    assert_eq!(defs.len(), 2);
    assert!(defs[0].test_only && !defs[1].test_only);
    let signature = defs[0].signature.clone();
    assert_eq!(signature, defs[1].signature);
    for argv in ARGV_FORMS {
        let _args =
            crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, argv);
        assert!(route_test_only(&defs, "health", "get", &signature, true));
        assert!(!route_test_only(&defs, "health", "get", &signature, false));
    }
}

/// The document of an identical-signature twin pair is the production twin's
/// route in every build (round-45 MR1-M1): under `cargo test` and under
/// rust-analyzer the stored entry is the `cfg(test)` stub, and the production
/// fn is read from the file (`documented_twins`), so `GET /twin-health` is
/// documented and `/stub` is not. The production build stores the production
/// twin, which is documented as is. Both argv forms give the same document.
#[test]
fn identical_twins_document_the_production_route_under_both_argv_forms() {
    let krate = crate_with(&[("twin_health.rs", TWIN_HEALTH)]);
    let src = krate.path().join("src");
    let file = src.join("twin_health.rs");
    let file_path = file.display().to_string();
    let signature = fn_definitions(&file)[0].signature.clone();
    let production_build = [stored_health(&file_path, &signature, false, None)];
    let test_build = [stored_health(&file_path, &signature, true, Some("/stub"))];
    let documented = |storage: &[StoredRouteInfo]| -> Vec<(String, String)> {
        crate::schema_macro::file_cache::bump_epoch();
        let (metadata, _) = crate::collector::collect_metadata(&src, "routes", storage).unwrap();
        documented_view(&metadata)
            .routes
            .iter()
            .map(|route| (route.method.clone(), route.path.clone()))
            .collect()
    };
    let production_document = vec![("get".to_string(), "/twin-health".to_string())];
    for argv in ARGV_FORMS {
        let _args =
            crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, argv);
        assert_eq!(documented(&production_build), production_document, "{argv}");
        assert_eq!(documented(&test_build), production_document, "{argv}");
    }

    // The test build still mounts its stub, and records only that entry.
    crate::schema_macro::file_cache::bump_epoch();
    let (metadata, _) = crate::collector::collect_metadata(&src, "routes", &test_build).unwrap();
    assert_eq!(metadata.routes.len(), 1);
    assert_eq!(metadata.routes[0].path, "/twin-health/stub");
    assert_eq!(metadata.test_only_routes.len(), 1);
    assert_eq!(metadata.documented_twins.len(), 1);
    assert!(!metadata.documented_twins[0].path.contains("stub"));
}
