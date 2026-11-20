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
// A glob of `super` in a file module may bring the struct in from a module
// this lookup cannot follow (`pub use super::*;`): the struct is nominal, not
// the crate alias. The qualifier is `crate::`: a bare `v2::` is not a crate
// module to the expansion.
#[case::super_glob_reexport_is_nominal(
    &[("lib.rs", "pub struct ApiResponse<T>(pub T); mod routes; pub mod v2;"), ("v2.rs", "pub use super::*;"), ROUTES],
    "pub fn h() {}",
    "crate::v2::ApiResponse<User>",
    false
)]
// A glob of an unresolved chain (`super::super::*`, deeper than one module
// level) may bring the struct in from a module this lookup cannot follow: the
// struct is nominal, not the crate alias.
#[case::unresolved_super_chain_glob_is_nominal(
    &[("lib.rs", "mod routes; pub mod v2;"), ("v2.rs", "pub use super::super::*;"), ROUTES],
    "pub fn h() {}",
    "crate::v2::ApiResponse<User>",
    false
)]
// A grouped re-export of a renamed struct (`use crate::{m::X as Y};`) names
// that struct too, so the same-named alias elsewhere is unrelated.
#[case::grouped_renamed_reexport_is_nominal(
    &[("lib.rs", "mod models; mod routes; pub use crate::{models::Envelope as ApiResponse};"), MODELS, ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
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
// Round 33 regression: a PRIVATE `use super::*;` brings the parent's names into
// its module only (here the handler's `users` module, whose parent is the
// file's top level holding the alias), so it still reaches the parent's alias.
// `pub(crate)` re-exports like `pub`: the struct stays nominal.
#[case::private_super_glob_keeps_the_parent_alias(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { use super::*; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    true
)]
#[case::pub_crate_super_glob_reexport_is_nominal(
    &[("lib.rs", "pub struct ApiResponse<T>(pub T); mod routes; pub mod v2 { pub(crate) use super::*; }"), ROUTES],
    "pub fn h() {}",
    "crate::v2::ApiResponse<User>",
    false
)]
// Round 34: `use a::X as Name;` imports `X`, so an alias stays expandable under
// the rename, and a struct `X` stays the return type itself.
#[case::renamed_alias_is_followed(
    &[("lib.rs", "mod models; mod routes;"), MODELS, ROUTES],
    "use crate::routes::ApiResponse as Api; pub fn h() {}",
    "Api<User>",
    true
)]
#[case::renamed_struct_stays_nominal(
    &[("lib.rs", "mod models; mod routes;"), MODELS, ROUTES],
    "use crate::models::ApiResponse as Api; pub fn h() {}",
    "Api<User>",
    false
)]
// Round 35: a `pub` rename of an ALIAS re-exports that alias, so it expands
// under the new name, from a qualified path or through a handler's import.
#[case::renamed_alias_reexport_is_followed(
    &[("lib.rs", "mod error; pub use error::AppResult as ApiResponse;"), ("error.rs", "pub type AppResult<T> = Result<Json<T>, AppErr>;")],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    true
)]
#[case::handler_imports_renamed_alias_reexport(
    &[("lib.rs", "mod error; pub use error::AppResult as ApiResponse;"), ("error.rs", "pub type AppResult<T> = Result<Json<T>, AppErr>;")],
    "use crate::ApiResponse; pub fn h() {}",
    "ApiResponse<User>",
    true
)]
// Round 35: a PRIVATE `use super::X;` is no re-export (like the private
// `use super::*;` above): it brings the parent's alias into its module only.
#[case::private_named_super_keeps_the_parent_alias(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { use super::ApiResponse; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    true
)]
// The `pub` form re-exports the parent's name: it stays nominal.
#[case::pub_named_super_reexport_stays_nominal(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { pub use super::ApiResponse; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    false
)]
// Round 35: a `pub` import of the parent's name in any form (a group, an
// `X as X` self rename) is a re-export, so it stays nominal.
#[case::pub_group_named_super_reexport_stays_nominal(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { pub use super::{ApiResponse}; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    false
)]
#[case::pub_self_renamed_super_reexport_stays_nominal(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { pub use super::ApiResponse as ApiResponse; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    false
)]
// Round 35: a private named `use super::X;` decides nothing even beside a
// public glob, which names no single item.
#[case::private_named_super_beside_a_public_glob(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { pub use super::*; use super::ApiResponse; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    true
)]
// A `pub` rename of an item another crate provides stays nominal (no alias
// of this crate is followed under that name).
#[case::external_renamed_reexport_is_nominal(
    &[("lib.rs", "mod routes; pub use axum_extra::Envelope as ApiResponse;"), ROUTES],
    "pub fn h() {}",
    "crate::ApiResponse<User>",
    false
)]
// Round 35: a `pub` rename from `super` names an item this lookup cannot
// follow, so it stays nominal.
#[case::pub_super_renamed_reexport_stays_nominal(
    &[("lib.rs", "")],
    "pub type ApiResponse<T> = Result<Json<T>, UsersErr>; pub mod users { pub use super::Envelope as ApiResponse; pub fn h() {} }",
    "crate::users::ApiResponse<User>",
    false
)]
// A named re-export through an unresolved `super::super` chain stays nominal.
#[case::unresolved_super_chain_named_is_nominal(
    &[("lib.rs", "mod routes; pub mod v2;"), ("v2.rs", "pub use super::super::ApiResponse;"), ROUTES],
    "pub fn h() {}",
    "crate::v2::ApiResponse<User>",
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
