//! Same-method `#[cfg(test)]` / `#[cfg(not(test))]` twins whose return type is
//! a return-type alias (round-43 R43-MR0-L1 / R43-MR1-M1): the storage holds the
//! alias-expanded signature, so the source of each twin is expanded the same
//! way before it is compared.

use std::fs;
use std::path::Path;

use super::tests::crate_with;
use super::*;

/// The storage form (`fn_sig_str`) of the `index`-th fn of `file`, as the
/// `#[route]` expansion of that fn stores it.
fn stored_form(file: &Path, index: usize) -> String {
    let ast = syn::parse_file(&fs::read_to_string(file).unwrap()).unwrap();
    let fns: Vec<&syn::ItemFn> = ast
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) => Some(function),
            _ => None,
        })
        .collect();
    let sig = crate::route_impl::stored_signature(&fns[index].sig, Some(file));
    quote::quote!(#sig).to_string()
}

/// A test-only and a production twin returning the alias with different
/// arguments: each build's stored entry (the alias-expanded form of its own
/// twin) is matched to that twin, so the stub is test-only and the production
/// fn is not.
#[test]
fn alias_twins_differing_in_the_alias_argument_are_told_apart() {
    let krate = crate_with(&[(
        "alias_twins.rs",
        "pub type ApiResult<T> = Result<Json<T>, AppError>;\n\
         #[cfg(test)]\n#[route(get)]\npub async fn list() -> ApiResult<TestUser> { todo!() }\n\
         #[cfg(not(test))]\n#[route(get)]\npub async fn list() -> ApiResult<User> { todo!() }\n",
    )]);
    let file = krate.path().join("src").join("alias_twins.rs");
    let defs = fn_definitions(&file);
    assert_eq!(defs.len(), 2);
    let stored_test = stored_form(&file, 0);
    let stored_prod = stored_form(&file, 1);
    assert_ne!(stored_test, stored_prod);
    assert!(route_test_only(&defs, "list", "get", &stored_test, false));
    assert!(!route_test_only(&defs, "list", "get", &stored_prod, false));
}

/// Identical-signature alias twins (`#[cfg(test)]` and `#[cfg(not(test))]` of one
/// fn returning `ApiResult<Vec<User>>`): the stored entry is the twin of the
/// build that stored it, so the verdict is that entry's own `cfg` (the twin
/// rule), not a signature mismatch that empties the match. No argv is read.
#[test]
fn identical_alias_twins_follow_the_build_that_stored_them() {
    let krate = crate_with(&[(
        "alias_same.rs",
        "pub type ApiResult<T> = Result<Json<T>, AppError>;
         #[cfg(test)]
#[route(get, path = \"/stub\")]
pub async fn list() -> ApiResult<Vec<User>> { todo!() }
         #[cfg(not(test))]
#[route(get)]
pub async fn list() -> ApiResult<Vec<User>> { todo!() }
",
    )]);
    let file = krate.path().join("src").join("alias_same.rs");
    let defs = fn_definitions(&file);
    assert_eq!(defs.len(), 2);
    let stored = stored_form(&file, 0);
    assert_eq!(stored, stored_form(&file, 1));
    assert!(route_test_only(&defs, "list", "get", &stored, true));
    assert!(!route_test_only(&defs, "list", "get", &stored, false));
}
