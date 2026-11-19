//! Re-exported names of the return-type alias lookup: a `pub use` (or a glob)
//! of a struct / enum / union is that nominal type, so a same-named alias
//! elsewhere in the crate is unrelated; a re-export of an alias keeps the
//! unique-definition fallback (sidecar of `result_alias`; `use super::*`
//! semantics).

use rstest::rstest;

use super::*;

/// A struct `models.rs` declares.
const MODELS: (&str, &str) = ("models.rs", "pub struct ApiResponse<T> { pub data: T }");
/// A route module's crate-wide alias of the same name.
const ROUTES: (&str, &str) = (
    "routes.rs",
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>;",
);
/// An alias another module declares (the only definition in these crates).
const LEGACY: (&str, &str) = (
    "legacy.rs",
    "pub type ApiResponse<T> = Result<Json<T>, LegacyErr>;",
);

/// Whether the return type `ret` of `handler` (in a handler file) expands in a
/// crate laid out as `files` (src-relative paths).
fn expands_in_crate(files: &[(&str, &str)], handler: &str, ret: &str) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    for (path, content) in files {
        std::fs::write(src.join(path), content).unwrap();
    }
    crate::schema_macro::file_cache::bump_epoch();
    let file: syn::File = syn::parse_str(handler).unwrap();
    let mut sig: syn::Signature = syn::parse_str(&format!("fn h() -> {ret}")).unwrap();
    expand_return_type_alias(&mut sig, None, Some(&file), Some(src))
}

/// Round 28: a struct the crate root or a named module only re-exports is the
/// return type itself, not the route module's alias. An alias re-export keeps
/// the unique-definition fallback; another crate's item, a rename, a `super`
/// import, and a re-export cycle are never expanded to that alias.
#[rstest]
#[case::crate_root_reexport(
    &[("lib.rs", "mod models; mod routes; pub use models::ApiResponse;"), MODELS, ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
#[case::crate_root_glob_reexport(
    &[("lib.rs", "mod models; mod routes; pub use models::*;"), MODELS, ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
#[case::module_reexport(
    &[("lib.rs", "mod models; mod routes; mod v2 { pub use super::models::ApiResponse; }"), MODELS, ROUTES],
    "pub fn h() {}",
    "v2::ApiResponse<User>",
    false
)]
#[case::module_glob_reexport(
    &[("lib.rs", "mod models; mod routes; mod v2 { pub use super::models::*; }"), MODELS, ROUTES],
    "pub fn h() {}",
    "v2::ApiResponse<User>",
    false
)]
#[case::handler_imports_reexport(
    &[("lib.rs", "mod models; mod routes; pub use models::ApiResponse;"), MODELS, ROUTES],
    "use crate::ApiResponse; pub fn h() {}",
    "ApiResponse<User>",
    false
)]
#[case::reexported_alias_keeps_fallback(
    &[("lib.rs", "mod legacy; pub use legacy::ApiResponse;"), LEGACY],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    true
)]
#[case::glob_reexported_alias_keeps_fallback(
    &[("lib.rs", "mod legacy; pub use legacy::*;"), LEGACY],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    true
)]
#[case::external_reexport(
    &[("lib.rs", "mod routes; pub use axum_extra::ApiResponse;"), ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
#[case::renamed_reexport(
    &[("lib.rs", "mod models; mod routes; pub use models::Envelope as ApiResponse;"), MODELS, ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
#[case::self_rename_import_is_nominal(
    &[("lib.rs", "mod models; mod routes;"), MODELS, ROUTES],
    "use crate::models::ApiResponse as ApiResponse; pub fn h() {}",
    "ApiResponse<User>",
    false
)]
#[case::self_rename_in_group_is_nominal(
    &[("lib.rs", "mod models; mod routes;"), MODELS, ROUTES],
    "use crate::{models::ApiResponse as ApiResponse}; pub fn h() {}",
    "ApiResponse<User>",
    false
)]
#[case::self_rename_after_an_unrelated_glob(
    &[("lib.rs", "mod models; mod routes;"), MODELS, ROUTES],
    "use std::collections::*; use crate::models::ApiResponse as ApiResponse; pub fn h() {}",
    "ApiResponse<User>",
    false
)]
#[case::super_reexport_in_module(
    &[("lib.rs", "mod routes; mod v2 { pub use super::ApiResponse; }"), ROUTES],
    "pub fn h() {}",
    "v2::ApiResponse<User>",
    false
)]
#[case::reexport_cycle(
    &[
        ("lib.rs", "mod a; mod b; mod routes; pub use crate::a::ApiResponse;"),
        ("a.rs", "pub use crate::b::ApiResponse;"),
        ("b.rs", "pub use crate::a::ApiResponse;"),
        ROUTES,
    ],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
fn reexports_decide_the_nominal_lookup(
    #[case] files: &[(&str, &str)],
    #[case] handler: &str,
    #[case] ret: &str,
    #[case] expands: bool,
) {
    assert_eq!(expands_in_crate(files, handler, ret), expands);
}
