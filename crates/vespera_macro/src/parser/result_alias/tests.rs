//! Return-type alias expansion: generic substitution, scoping rules (own
//! module, qualified paths, imports, test-only code) and crate-wide
//! lookups.

use rstest::rstest;

use super::*;

fn expand(file_src: &str, ret: &str) -> String {
    let file: syn::File = syn::parse_str(file_src).unwrap();
    let mut sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    let before = {
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    let changed = expand_return_type_alias(&mut sig, None, Some(&file), None);
    let output = &sig.output;
    let after = quote::quote!(#output).to_string();
    // The return value reports exactly whether the type changed (round 15:
    // a self-recursive alias reported `true` for an unchanged type).
    assert_eq!(changed, after != before, "{file_src} {ret}");
    after
}

#[rstest]
#[case::generic_result_alias(
    "pub type ApiResult<T> = Result<Json<T>, AppError>;",
    "-> ApiResult<User>",
    "-> Result < Json < User > , AppError >"
)]
#[case::default_param(
    "type R<T, E = AppError> = Result<Json<T>, E>;",
    "-> R<User>",
    "-> Result < Json < User > , AppError >"
)]
#[case::default_names_earlier_param(
    "type R<T, E = Wrapper<T>> = Result<Json<T>, E>;",
    "-> R<User>",
    "-> Result < Json < User > , Wrapper < User > >"
)]
#[case::default_names_earlier_defaulted_param(
    "type R<T, U = Vec<T>, E = Wrapper<U>> = Result<Json<U>, E>;",
    "-> R<User>",
    "-> Result < Json < Vec < User > > , Wrapper < Vec < User > > >"
)]
#[case::user_result_alias(
    "type Result<T> = std::result::Result<T, AppError>;",
    "-> Result<Json<i32>>",
    "-> std :: result :: Result < Json < i32 > , AppError >"
)]
#[case::alias_of_alias(
    "type Inner<T> = Result<Json<T>, E>; type Outer = Inner<u8>;",
    "-> Outer",
    "-> Result < Json < u8 > , E >"
)]
#[case::alias_in_inline_module(
    "mod error { pub type ApiResult<T> = Result<T, AppError>; }",
    "-> ApiResult<Json<u8>>",
    "-> Result < Json < u8 > , AppError >"
)]
#[case::std_result_untouched(
    "type Result<T> = std::result::Result<T, AppError>;",
    "-> Result<Json<i32>, String>",
    "-> Result < Json < i32 > , String >"
)]
#[case::wrapper_untouched("type Json = Bad;", "-> Json<u8>", "-> Json < u8 >")]
#[case::too_many_args("type R<T> = Result<T, E>;", "-> R<A, B>", "-> R < A , B >")]
#[case::missing_arg_without_default("type R<T, E> = Result<T, E>;", "-> R<A>", "-> R < A >")]
#[case::unknown_alias("", "-> ApiResult<User>", "-> ApiResult < User >")]
#[case::self_recursive_stops("type Result<T> = Result<T>;", "-> Result<u8>", "-> Result < u8 >")]
#[case::cycle_back_to_start("type A = B; type B = A;", "-> A", "-> A")]
#[case::qualified_self("type F = u8;", "-> <X as Tr>::F", "-> < X as Tr > :: F")]
#[case::non_path("type F = u8;", "-> (F, F)", "-> (F , F)")]
#[case::result_without_args("type Result = u8;", "-> Result", "-> Result")]
#[case::lifetime_arg_ignored(
    "type R<'a, T> = Result<Json<T>, E>;",
    "-> R<'static, u8>",
    "-> Result < Json < u8 > , E >"
)]
// Another crate's item is never expanded through a same-named alias.
#[case::qualified_external(
    "type Result<T> = std::result::Result<T, AppError>;",
    "-> axum::response::Result<Json<u8>>",
    "-> axum :: response :: Result < Json < u8 > >"
)]
#[case::leading_colon(
    "type ApiResult<T> = Result<T, E>;",
    "-> ::app::ApiResult<u8>",
    "-> :: app :: ApiResult < u8 >"
)]
#[case::crate_path(
    "type ApiResult<T> = Result<T, E>;",
    "-> crate::ApiResult<u8>",
    "-> Result < u8 , E >"
)]
#[case::declared_module_path(
    "mod error { pub type ApiResult<T> = Result<T, E>; }",
    "-> error::ApiResult<u8>",
    "-> Result < u8 , E >"
)]
#[case::locally_imported_module_path(
    "use crate::errors; mod m { pub mod errors { pub type ApiResult<T> = Result<T, E>; } }",
    "-> errors::ApiResult<u8>",
    "-> Result < u8 , E >"
)]
// A qualified path names the alias of the module it names, never the
// handler's own same-named alias.
#[case::qualified_skips_handler_module(
    "type ApiResult<T> = Result<Json<T>, LocalErr>; mod v2 { pub type ApiResult<T> = Result<Json<T>, V2Err>; }",
    "-> v2::ApiResult<u8>",
    "-> Result < Json < u8 > , V2Err >"
)]
// Not declared in the named module (a re-export): the unique definition.
#[case::qualified_reexport(
    "mod v2 { pub use super::inner::*; } mod inner { pub type ApiResult<T> = Result<T, E>; }",
    "-> v2::ApiResult<u8>",
    "-> Result < u8 , E >"
)]
// Test-only code never defines a handler's alias.
#[case::cfg_test_module(
    "#[cfg(test)] mod tests { pub type ApiResult<T> = Result<T, TestError>; }",
    "-> ApiResult<u8>",
    "-> ApiResult < u8 >"
)]
#[case::cfg_test_item(
    "#[cfg(test)] type ApiResult<T> = Result<T, TestError>;",
    "-> ApiResult<u8>",
    "-> ApiResult < u8 >"
)]
// The handler's own module wins over another module's same-named alias.
#[case::handler_module_scope(
    "mod b { type R = Json<i8>; } mod a { type R = Json<u8>; pub fn handler() {} }",
    "-> R",
    "-> Json < u8 >"
)]
// Round 12: a same-named helper in an inline module (listed first) or in a
// test-only module is not the handler.
#[case::top_level_handler_before_inline_helper(
    "mod helpers { type R = Json<i8>; fn handler() {} } type R = Json<u8>; pub fn handler() {}",
    "-> R",
    "-> Json < u8 >"
)]
#[case::test_module_helper_skipped(
    "#[cfg(test)] mod tests { type R = Json<i8>; fn handler() {} } type R = Json<u8>;",
    "-> R",
    "-> Json < u8 >"
)]
// An external import is checked before another module's alias.
#[case::import_before_other_module(
    "use axum::response::Result; mod inner { pub type Result<T> = std::result::Result<T, E>; }",
    "-> Result<Json<u8>>",
    "-> Result < Json < u8 > >"
)]
// Two different definitions in other modules are ambiguous.
#[case::ambiguous_modules(
    "mod a { type R = Json<u8>; } mod b { type R = Json<i8>; }",
    "-> R",
    "-> R"
)]
fn expands_aliases(#[case] file_src: &str, #[case] ret: &str, #[case] expected: &str) {
    assert_eq!(expand(file_src, ret), expected);
}

#[test]
fn qualified_path_without_file_is_external() {
    let mut sig: syn::Signature = syn::parse_str("fn h() -> error::ApiResult<u8>").unwrap();
    assert!(!expand_return_type_alias(&mut sig, None, None, None));
}

#[rstest]
#[case::bare("ApiResult<u8>", None)]
#[case::module("v2::ApiResult<u8>", Some(Qualifier::Module("v2".to_string())))]
#[case::innermost(
    "crate::api::r#v2::ApiResult<u8>",
    Some(Qualifier::Module("v2".to_string()))
)]
#[case::crate_root("crate::ApiResult<u8>", Some(Qualifier::CrateRoot))]
#[case::super_module("super::ApiResult<u8>", Some(Qualifier::Super))]
#[case::self_module("self::ApiResult<u8>", None)]
#[case::super_chain("super::super::ApiResult<u8>", None)]
#[case::non_path("(u8, u8)", None)]
fn alias_qualifiers(#[case] ty: &str, #[case] expected: Option<Qualifier>) {
    let ty: Type = syn::parse_str(ty).unwrap();
    assert_eq!(alias_qualifier(&ty), expected);
}

/// After `use anyhow;`, `anyhow::Result<T>` is the other crate's, never the
/// crate's own `type Result<T>`.
#[test]
fn bare_extern_crate_import_is_external() {
    let file: syn::File =
        syn::parse_str("use anyhow; type Result<T> = std::result::Result<T, E>;").unwrap();
    let mut sig: syn::Signature = syn::parse_str("fn h() -> anyhow::Result<u8>").unwrap();
    assert!(!expand_return_type_alias(&mut sig, None, Some(&file), None));
}

/// `crate::ApiResult` is the crate root's alias and `super::ApiResult` the
/// parent module's — never the handler module's own same-named alias.
#[test]
fn crate_and_super_paths_skip_the_handler_module() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("lib.rs"),
        "mod routes; pub type ApiResult<T> = Result<Json<T>, AppErr>;",
    )
    .unwrap();
    std::fs::create_dir(src.join("routes")).unwrap();
    std::fs::write(
        src.join("routes").join("mod.rs"),
        "pub mod users; pub type ApiResult<T> = Result<Json<T>, RoutesErr>;",
    )
    .unwrap();
    let handler = "pub type ApiResult<T> = Result<Json<T>, UsersErr>;";
    std::fs::write(src.join("routes").join("users.rs"), handler).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |file_src: &str, ret: &str, src_dir: Option<&Path>| {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), src_dir);
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    assert_eq!(
        expand_in(handler, "-> crate::ApiResult<u8>", Some(src)),
        "-> Result < Json < u8 > , AppErr >"
    );
    // The parent of a file module is another file: any definition but the
    // handler module's own; two of them are ambiguous.
    assert_eq!(
        expand_in(handler, "-> super::ApiResult<u8>", Some(src)),
        "-> super :: ApiResult < u8 >"
    );
    assert_eq!(
        expand_in(
            "type ApiResult<T> = Result<T, Own>; mod inner { type Other = u8; }",
            "-> super::ApiResult<u8>",
            None
        ),
        "-> super :: ApiResult < u8 >"
    );
    // The parent's `ApiResult` is a struct: never the handler module's own
    // alias either.
    let other = tempfile::tempdir().unwrap();
    let other_src = other.path();
    std::fs::write(other_src.join("lib.rs"), "mod routes;").unwrap();
    std::fs::create_dir(other_src.join("routes")).unwrap();
    std::fs::write(
        other_src.join("routes").join("mod.rs"),
        "pub mod users; pub struct ApiResult<T>(T);",
    )
    .unwrap();
    let own = "type ApiResult<T> = Result<T, Own>;";
    std::fs::write(other_src.join("routes").join("users.rs"), own).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(
        expand_in(own, "-> super::ApiResult<u8>", Some(other_src)),
        "-> super :: ApiResult < u8 >"
    );
    // An inline parent in the handler's file.
    assert_eq!(
        expand_in(
            "type ApiResult<T> = Result<T, Parent>; mod m { type ApiResult<T> = Result<T, Child>; pub fn h() {} }",
            "-> super::ApiResult<u8>",
            None
        ),
        "-> Result < u8 , Parent >"
    );
    assert_eq!(
        expand_in(
            "mod m { type ApiResult<T> = Result<T, Child>; pub fn h() {} } mod other { pub type ApiResult<T> = Result<T, Other>; }",
            "-> super::ApiResult<u8>",
            None
        ),
        "-> Result < u8 , Other >"
    );
    // No crate root definition (none under `src/`): the unique one.
    assert_eq!(
        expand_in(
            "type ApiResult<T> = Result<T, E>;",
            "-> crate::ApiResult<u8>",
            None
        ),
        "-> Result < u8 , E >"
    );
}

#[rstest]
#[case::external("use axum::response::Result;", Some(Import::External))]
#[case::local("use crate::error::Result;", Some(Import::Local))]
#[case::self_root("use self::x::Result;", Some(Import::Local))]
#[case::leading_colon("use ::std::result::Result;", Some(Import::External))]
#[case::group_rename("use a::{b, c::D as Result};", Some(Import::External))]
// A path-less `use name;` names an extern crate, unless it is a module of
// this crate.
#[case::bare_name("use Result;", Some(Import::External))]
#[case::bare_rename("use anyhow as Result;", Some(Import::External))]
#[case::bare_local_module("mod error; use error as Result;", Some(Import::Local))]
// Only another crate's glob can bring the name in.
#[case::glob("use a::*;", Some(Import::External))]
#[case::local_glob("use crate::error::*;", None)]
#[case::external_and_local_globs("use axum::response::*; use crate::error::*;", None)]
#[case::named_before_glob(
    "use axum::response::*; use crate::error::Result;",
    Some(Import::Local)
)]
#[case::other_name("use axum::response::Html;", None)]
// A `use` inside another module does not reach this one.
#[case::inline_module("mod m { use axum::Result; }", None)]
#[case::declared_module("mod error; use error::Result;", Some(Import::Local))]
#[case::module_without_body("mod m; struct S;", None)]
fn import_kinds(#[case] file_src: &str, #[case] expected: Option<Import>) {
    let file: syn::File = syn::parse_str(file_src).unwrap();
    assert_eq!(import_kind(&file.items, "Result"), expected);
}

#[rstest]
// Only the module's own items: a nested module is another scope.
#[case::nested("mod a { mod error {} }", false)]
#[case::declared("mod error;", true)]
#[case::other("mod a; struct error;", false)]
fn module_declarations(#[case] file_src: &str, #[case] expected: bool) {
    let file: syn::File = syn::parse_str(file_src).unwrap();
    let name: syn::Ident = syn::parse_str("error").unwrap();
    assert_eq!(declares_module(&file.items, &name), expected);
}

/// `use axum::response::Result;` makes `Result<..>` axum's, so the
/// crate's own `type Result<T>` elsewhere under `src/` does not apply.
#[test]
fn externally_imported_name_skips_crate_wide_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("error.rs"),
        "pub type Result<T> = std::result::Result<T, AppError>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let handler_file: syn::File = syn::parse_str("use axum::response::Result;").unwrap();
    let mut sig: syn::Signature = syn::parse_str("fn h() -> Result<Json<u8>>").unwrap();
    assert!(!expand_return_type_alias(
        &mut sig,
        None,
        Some(&handler_file),
        Some(src)
    ));
    let local_file: syn::File = syn::parse_str("use crate::error::Result;").unwrap();
    assert!(expand_return_type_alias(
        &mut sig,
        None,
        Some(&local_file),
        Some(src)
    ));
}

/// A struct named like a crate-wide alias — declared in the handler's
/// module, or imported from the module (file or inline) that declares
/// it — is the return type itself; an import from a module without such
/// a struct still finds the alias.
#[rstest]
#[case::declared_here("pub struct ApiResponse<T>(T);", false)]
#[case::declared_enum_here("pub enum ApiResponse<T> { A(T) }", false)]
#[case::declared_union_here("pub union ApiResponse<T> { a: T }", false)]
#[case::test_only_struct("#[cfg(test)] pub struct ApiResponse<T>(T);", true)]
#[case::imported_from_file("use crate::models::ApiResponse;", false)]
#[case::imported_from_self("use self::ApiResponse;", true)]
#[case::imported_from_inline_module(
    "mod inner { pub struct ApiResponse<T>(T); } use inner::ApiResponse;",
    false
)]
#[case::imported_from_alias_module("use crate::legacy::ApiResponse;", true)]
#[case::imported_through_group("use crate::{models::{ApiResponse}};", false)]
// Round 13: a local rename names another item, never the unrelated
// crate-wide alias of the same name; `X as X` is no rename.
#[case::renamed_import("use crate::models::Other as ApiResponse;", false)]
#[case::renamed_in_group("use crate::{models::Envelope as ApiResponse};", false)]
#[case::renamed_to_itself("use crate::legacy::ApiResponse as ApiResponse;", true)]
#[case::renamed_other_name("use crate::models::Other as Unrelated;", true)]
// Round 12: `use crate::X;` / `use super::X;` name the crate root's / the
// parent module's struct too.
#[case::imported_from_crate_root("use crate::ApiResponse;", false)]
#[case::imported_from_parent_file("use super::ApiResponse;", false)]
#[case::imported_from_inline_parent(
    "pub struct ApiResponse<T>(T); mod m { use super::ApiResponse; pub fn h() {} }",
    false
)]
#[case::imported_from_inline_parent_without_struct(
    "pub struct Other; mod m { use super::ApiResponse; pub fn h() {} }",
    true
)]
// A local glob brings the struct in too; a glob of a module without one
// still reaches the crate-wide alias.
#[case::local_glob_of_declaring_module("use crate::models::*;", false)]
#[case::local_glob_of_other_module("use crate::legacy::*;", true)]
#[case::external_glob_is_not_local("use axum::response::*;", false)]
fn same_named_struct_stops_crate_wide_lookup(#[case] handler: &str, #[case] expands: bool) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("lib.rs"),
        "mod legacy; mod models; pub struct ApiResponse<T>(T);",
    )
    .unwrap();
    std::fs::write(
        src.join("legacy.rs"),
        "pub type ApiResponse<T> = Result<Json<T>, LegacyError>;",
    )
    .unwrap();
    std::fs::write(
        src.join("models.rs"),
        "pub struct ApiResponse<T> { data: T } mod nested { struct Unrelated; }",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let file: syn::File = syn::parse_str(handler).unwrap();
    let mut sig: syn::Signature = syn::parse_str("fn h() -> ApiResponse<u8>").unwrap();
    assert_eq!(
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src)),
        expands
    );
}

/// `super::X` skips the handler module's own alias by file identity: a parent
/// module declaring the identical text is still the parent's alias, so it is
/// the one expanded (not dropped because its tokens equal the handler's).
#[test]
fn super_keeps_a_parent_alias_identical_to_the_own_one() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    let alias = "pub type ApiResult<T> = Result<Json<T>, Shared>;";
    std::fs::write(src.join("lib.rs"), "mod routes;").unwrap();
    std::fs::create_dir(src.join("routes")).unwrap();
    std::fs::write(
        src.join("routes").join("mod.rs"),
        format!("pub mod users; {alias}"),
    )
    .unwrap();
    let users = src.join("routes").join("users.rs");
    std::fs::write(&users, alias).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let file: syn::File = syn::parse_str(alias).unwrap();
    let mut sig: syn::Signature = syn::parse_str("fn h() -> super::ApiResult<u8>").unwrap();
    assert!(expand_return_type_alias(
        &mut sig,
        Some(&users),
        Some(&file),
        Some(src)
    ));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Result < Json < u8 > , Shared >"
    );
}

/// Only `src/lib.rs` / `src/main.rs` is the crate root: a `main.rs` in a
/// subdirectory is an ordinary module, so `crate::ApiResult` names the
/// root's alias alone (it is not ambiguous with the nested `main.rs`).
#[test]
fn nested_main_file_is_not_the_crate_root() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("lib.rs"),
        "pub mod admin; pub type ApiResult<T> = Result<Json<T>, AppErr>;",
    )
    .unwrap();
    std::fs::create_dir(src.join("admin")).unwrap();
    std::fs::write(
        src.join("admin").join("main.rs"),
        "pub type ApiResult<T> = Result<Json<T>, AdminErr>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let file: syn::File = syn::parse_str("pub fn unrelated() {}").unwrap();
    let mut sig: syn::Signature = syn::parse_str("fn h() -> crate::ApiResult<u8>").unwrap();
    assert!(expand_return_type_alias(
        &mut sig,
        None,
        Some(&file),
        Some(src)
    ));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Result < Json < u8 > , AppErr >"
    );
}

/// An alias of an alias is resolved where the outer alias is defined
/// (the handler's `use axum::response::Result` does not apply inside
/// `error.rs`); `use error::Result` next to `mod error;` is this crate's
/// `Result`; test-only files (`#[cfg(test)] mod fixtures;`) define
/// nothing.
#[test]
fn crate_wide_aliases_resolve_in_their_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(src.join("lib.rs"), "mod error; #[cfg(test)] mod fixtures;").unwrap();
    std::fs::write(
        src.join("error.rs"),
        "pub type Result<T> = std::result::Result<T, AppError>;\npub type ApiResult<T> = Result<Json<T>>;",
    )
    .unwrap();
    std::fs::write(
        src.join("fixtures.rs"),
        "pub type ApiResult<T> = std::result::Result<T, TestError>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |file_src: &str, ret: &str| {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    assert_eq!(
        expand_in("use axum::response::Result;", "-> ApiResult<u8>"),
        "-> std :: result :: Result < Json < u8 > , AppError >"
    );
    assert_eq!(
        expand_in("mod error; use error::Result;", "-> Result<u8>"),
        "-> std :: result :: Result < u8 , AppError >"
    );
}

/// `v2::ApiResult` is the alias `v2.rs` declares, not the handler
/// module's; an external glob (`use axum::response::*;`) makes an
/// unimported `Result` axum's, so the crate's `type Result<T>` does not
/// apply.
#[test]
fn qualified_and_glob_imported_names_resolve_their_origin() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::create_dir(src.join("api")).unwrap();
    std::fs::write(
        src.join("api").join("mod.rs"),
        "pub type ApiResult<T> = Result<Json<T>, ApiErr>;",
    )
    .unwrap();
    std::fs::write(
        src.join("v2.rs"),
        "pub type ApiResult<T> = Result<Json<T>, V2Err>;",
    )
    .unwrap();
    std::fs::write(
        src.join("error.rs"),
        "pub type Result<T> = std::result::Result<T, AppError>;",
    )
    .unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub type ApiResult<T> = Result<Json<T>, RootErr>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |file_src: &str, ret: &str| {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    let handler = "mod v2; mod api; type ApiResult<T> = Result<Json<T>, LocalErr>;";
    assert_eq!(
        expand_in(handler, "-> v2::ApiResult<u8>"),
        "-> Result < Json < u8 > , V2Err >"
    );
    assert_eq!(
        expand_in(handler, "-> api::ApiResult<u8>"),
        "-> Result < Json < u8 > , ApiErr >"
    );
    assert_eq!(
        expand_in("use axum::response::*;", "-> Result<Json<u8>>"),
        "-> Result < Json < u8 > >"
    );
    assert_eq!(
        expand_in(
            "use axum::response::*; use crate::error::*;",
            "-> Result<u8>"
        ),
        "-> std :: result :: Result < u8 , AppError >"
    );
}

/// The crate-wide scan for an alias name runs once while `src/` is
/// unchanged, and again after an edit or a new file (rust-analyzer's
/// long-lived server sees both between invocations).
#[test]
fn crate_wide_scan_is_memoised_until_src_changes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("error.rs"),
        "pub type ApiResult<T> = Result<Json<T>, AppError>;",
    )
    .unwrap();
    let expand_in = || {
        crate::schema_macro::file_cache::bump_epoch();
        let mut sig: syn::Signature = syn::parse_str("fn h() -> ApiResult<u8>").unwrap();
        expand_return_type_alias(&mut sig, None, None, Some(src));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    let scans = || SCANS.with(std::cell::Cell::get);
    let before = scans();
    for _ in 0..3 {
        assert_eq!(expand_in(), "-> Result < Json < u8 > , AppError >");
    }
    assert_eq!(scans() - before, 1);

    std::fs::write(
        src.join("error.rs"),
        "pub type ApiResult<T> = Result<Json<T>, ChangedError>;",
    )
    .unwrap();
    assert_eq!(expand_in(), "-> Result < Json < u8 > , ChangedError >");
    assert_eq!(scans() - before, 2);

    std::fs::write(
        src.join("other.rs"),
        "pub type ApiResult<T> = Result<T, OtherError>;",
    )
    .unwrap();
    // Two different definitions: ambiguous, left unexpanded.
    assert_eq!(expand_in(), "-> ApiResult < u8 >");
    assert_eq!(scans() - before, 3);
}

#[test]
fn default_return_is_untouched() {
    let mut sig: syn::Signature = syn::parse_str("fn handler()").unwrap();
    assert!(!expand_return_type_alias(&mut sig, None, None, None));
}

#[test]
fn crate_wide_lookup_finds_unique_alias_and_rejects_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("error.rs"),
        "pub type ApiResult<T> = Result<Json<T>, AppError>;\npub type Dup = A;",
    )
    .unwrap();
    std::fs::write(src.join("other.rs"), "pub type Dup = B; // type ApiResult").unwrap();
    std::fs::write(
        src.join("same.rs"),
        "pub type ApiResult<T> = Result<Json<T>, AppError>;",
    )
    .unwrap();
    std::fs::write(src.join("broken.rs"), "type Dup = ").unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let mut sig: syn::Signature = syn::parse_str("fn h() -> ApiResult<User>").unwrap();
    assert!(expand_return_type_alias(&mut sig, None, None, Some(src)));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Result < Json < User > , AppError >"
    );

    let mut sig: syn::Signature = syn::parse_str("fn h() -> Dup").unwrap();
    assert!(!expand_return_type_alias(&mut sig, None, None, Some(src)));
}

/// Round 14: a plain `use` names the module the alias comes from (like a
/// qualified path), so a same-named alias elsewhere is not ambiguous; an
/// alias of a same-named alias in another module is followed (the cycle
/// guard keys on the definition, not the ident).
#[test]
fn imported_alias_resolves_its_module_and_same_named_chains_follow() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::create_dir(src.join("v1")).unwrap();
    std::fs::create_dir(src.join("v2")).unwrap();
    std::fs::write(
        src.join("v1").join("mod.rs"),
        "pub type ApiResult<T> = Result<Json<T>, V1Error>;",
    )
    .unwrap();
    std::fs::write(
        src.join("v2").join("mod.rs"),
        "pub type ApiResult<T> = Result<Json<T>, V2Error>;",
    )
    .unwrap();
    std::fs::write(
        src.join("error.rs"),
        "pub type Result<T, E = AppError> = std::result::Result<T, E>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |file_src: &str, ret: &str| {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    assert_eq!(
        expand_in("use crate::api::v2::ApiResult;", "-> ApiResult<u8>"),
        "-> Result < Json < u8 > , V2Error >"
    );
    assert_eq!(
        expand_in("use crate::v1::{ApiResult, Other};", "-> ApiResult<u8>"),
        "-> Result < Json < u8 > , V1Error >"
    );
    // Not declared in the imported module (a re-export): still ambiguous.
    assert_eq!(
        expand_in("use crate::prelude::ApiResult;", "-> ApiResult<u8>"),
        "-> ApiResult < u8 >"
    );
    // `use super::ApiResult;` from an inline module: the inline parent's.
    assert_eq!(
        expand_in(
            "type ApiResult<T> = Result<T, Parent>; mod m { use super::ApiResult; pub fn h() {} }",
            "-> ApiResult<u8>"
        ),
        "-> Result < u8 , Parent >"
    );
    // `type Result<T> = crate::error::Result<Json<T>>;` in the handler's
    // module is followed into `error.rs`'s own `Result`.
    assert_eq!(
        expand_in(
            "pub type Result<T> = crate::error::Result<Json<T>>;",
            "-> Result<u8>"
        ),
        "-> std :: result :: Result < Json < u8 > , AppError >"
    );
}
