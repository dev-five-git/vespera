//! Test-only files in the return-type alias lookup: a `#[cfg(test)] mod`
//! fixture file never supplies the nominal item a handler's `use super::`
//! names, so it cannot stop the crate's alias from expanding (sidecar of
//! `result_alias`; `use super::*` semantics).

use rstest::rstest;

use super::*;

/// Whether the return type `ret` of a handler at the crate root (importing
/// `use super::ApiResponse;`) expands in a crate laid out as `files`.
fn expands_in_crate(files: &[(&str, &str)], ret: &str) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    for (path, content) in files {
        std::fs::write(src.join(path), content).unwrap();
    }
    crate::schema_macro::file_cache::bump_epoch();
    let file: syn::File = syn::parse_str("use super::ApiResponse; pub fn h() {}").unwrap();
    let mut sig: syn::Signature = syn::parse_str(&format!("fn h() -> {ret}")).unwrap();
    expand_return_type_alias(&mut sig, None, Some(&file), Some(src))
}

/// Round 31: a struct a `#[cfg(test)]` file declares is not the nominal item
/// `super::ApiResponse` names, so the crate's alias still expands. A plain
/// (non-test) fixture module declaring the same struct does shadow it.
#[rstest]
#[case::test_only_fixture_file("#[cfg(test)] mod fixtures;", true)]
#[case::plain_fixture_file("mod fixtures;", false)]
fn test_only_files_do_not_shadow_the_alias(#[case] declaration: &str, #[case] expanded: bool) {
    let lib = format!("pub type ApiResponse<T> = Result<Json<T>, AppError>; {declaration}");
    let files = [
        ("lib.rs", lib.as_str()),
        ("fixtures.rs", "pub struct ApiResponse<T>(pub T);"),
    ];
    assert_eq!(expands_in_crate(&files, "ApiResponse<u8>"), expanded);
}
