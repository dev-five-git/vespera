//! Aliases inside a `Json` response body are expanded like the other response
//! positions, and request bodies are left as written (sidecar of
//! `result_alias`; `use super::*` semantics).

use rstest::rstest;

use super::*;

fn expand_return(file_src: &str, ret: &str) -> String {
    let file: syn::File = syn::parse_str(file_src).unwrap();
    let mut sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    expand_return_type_alias(&mut sig, None, Some(&file), None);
    let output = &sig.output;
    quote::quote!(#output).to_string()
}

/// `Json<UserList>` documents the aliased body: the alias is expanded inside
/// the wrapper, alone or in a `Result` / tuple.
#[rstest]
#[case::json_body(
    "type UserList = Vec<User>;",
    "-> Json<UserList>",
    "-> Json < Vec < User > >"
)]
#[case::json_generic_body(
    "type Page<T> = Vec<T>;",
    "-> Json<Page<User>>",
    "-> Json < Vec < User > >"
)]
#[case::json_body_in_result(
    "type UserList = Vec<User>;",
    "-> Result<Json<UserList>, AppError>",
    "-> Result < Json < Vec < User > > , AppError >"
)]
#[case::json_body_in_tuple(
    "type UserList = Vec<User>;",
    "-> (StatusCode, Json<UserList>)",
    "-> (StatusCode , Json < Vec < User > >)"
)]
#[case::json_body_alias_of_alias(
    "type Inner = Vec<User>; type UserList = Inner;",
    "-> Json<UserList>",
    "-> Json < Vec < User > >"
)]
// Only a `Json` body is a response position: other wrappers and plain
// arguments document no alias body, so they are left as written.
#[case::html_body_untouched("type Page = String;", "-> Html<Page>", "-> Html < Page >")]
#[case::vec_argument_untouched(
    "type UserList = Vec<User>;",
    "-> Vec<UserList>",
    "-> Vec < UserList >"
)]
fn expands_aliases_inside_json_bodies(
    #[case] file_src: &str,
    #[case] ret: &str,
    #[case] expected: &str,
) {
    assert_eq!(expand_return(file_src, ret), expected);
}

/// The gate reads the source file for an alias that is only a `Json` body.
#[rstest]
#[case::json_body("-> Json<UserList>", true)]
#[case::json_in_result("-> Result<Json<UserList>, AppError>", true)]
#[case::json_of_builtin("-> Json<u8>", false)]
#[case::html_body("-> Html<Page>", false)]
fn gate_follows_json_bodies(#[case] ret: &str, #[case] expected: bool) {
    let sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    assert_eq!(needs_alias_expansion(&sig), expected);
}

/// An alias in a `Json` body declared by another file of the crate is found
/// there too, the same as any other response position.
#[test]
fn crate_wide_lookup_expands_a_json_body_alias() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(src.join("models.rs"), "pub type UserList = Vec<User>;").unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let mut sig: syn::Signature = syn::parse_str("fn h() -> Json<UserList>").unwrap();
    assert!(expand_return_type_alias(&mut sig, None, None, Some(src)));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Json < Vec < User > >"
    );
}
