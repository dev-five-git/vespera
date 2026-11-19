//! Glob and qualifier imports of the return-type alias lookup, and one alias
//! name defined in several modules (sidecar of `result_alias`; `use super::*`
//! semantics).

use super::*;

/// `use crate::api::*;` brings the `v2` module of `api` into scope, so the
/// qualified `v2::ApiResult` is that module's alias, not another crate's
/// item (a local glob is no external name). An external glob alone still
/// makes the qualifier external, and so does a named import of it.
#[test]
fn local_glob_brings_a_qualifier_module_in() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::create_dir(src.join("api")).unwrap();
    std::fs::write(src.join("api").join("mod.rs"), "pub mod v2;").unwrap();
    std::fs::write(
        src.join("api").join("v2.rs"),
        "pub type ApiResult<T> = Result<Json<T>, V2Err>;",
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
        expand_in("use crate::api::*;", "-> v2::ApiResult<u8>"),
        "-> Result < Json < u8 > , V2Err >"
    );
    assert_eq!(
        expand_in("use axum::response::*;", "-> v2::ApiResult<u8>"),
        "-> v2 :: ApiResult < u8 >"
    );
    assert_eq!(
        expand_in("use crate::api::*; use other::v2;", "-> v2::ApiResult<u8>"),
        "-> v2 :: ApiResult < u8 >"
    );
}

/// An un-imported qualifier stays external under a local glob unless a glob
/// target really declares `mod <qualifier>`: `axum::` under `use crate::
/// models::*;` or `use super::*;` does not expand the crate's `type Result`.
#[test]
fn unimported_qualifier_under_a_local_glob_stays_external() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("error.rs"),
        "pub type Result<T> = std::result::Result<T, AppError>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |file_src: &str| {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature =
            syn::parse_str("fn h() -> axum::response::Result<Json<u8>>").unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    let unexpanded = "-> axum :: response :: Result < Json < u8 > >";
    assert_eq!(expand_in("use crate::models::*;"), unexpanded);
    assert_eq!(expand_in("use super::*;"), unexpanded);
}

/// The same alias tokens defined in two modules are two definitions, which
/// name their body's types per module: an alias found that way stays
/// unexpanded, until one of the two definitions is gone.
#[test]
fn same_tokens_in_two_modules_stay_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    let body = "pub type Api<T> = Result<Json<T>, AppError>;";
    std::fs::write(src.join("a.rs"), body).unwrap();
    std::fs::write(src.join("b.rs"), body).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let expand_in = |src_dir: &Path| {
        let file: syn::File = syn::parse_str("fn other() {}").unwrap();
        let mut sig: syn::Signature = syn::parse_str("fn h() -> Api<u8>").unwrap();
        expand_return_type_alias(&mut sig, None, Some(&file), Some(src_dir));
        let output = &sig.output;
        quote::quote!(#output).to_string()
    };
    assert_eq!(expand_in(src), "-> Api < u8 >");
    std::fs::remove_file(src.join("b.rs")).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(expand_in(src), "-> Result < Json < u8 > , AppError >");
}
