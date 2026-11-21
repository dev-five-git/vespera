//! The source pre-filter (`declares_type_named`) finds `type <name>` whatever
//! sits between the keyword and the name, comments included (sidecar of
//! `result_alias`; `use super::*` semantics).

use rstest::rstest;

use super::*;

#[rstest]
#[case::block_comment_between("type /* c */ Api = u8;", true)]
#[case::line_comment_between("type // c\nApi = u8;", true)]
#[case::comment_and_break("pub type /* a */\nApi<T> = T;", true)]
#[case::nested_module("mod inner { pub type Api = u8; }", true)]
#[case::string_mentions_pair("let s = \"type Api\";", false)]
#[case::comment_mentions_pair("// type Api = u8;", false)]
#[case::other_name_after_keyword("type Other = Api;", false)]
// Content the lexer rejects is not pre-filtered out: the parse decides.
#[case::unlexable_is_parsed("fn f() { \"unterminated", true)]
fn declares_type_named_reads_through_comments(#[case] content: &str, #[case] expected: bool) {
    assert_eq!(declares_type_named(content, "Api"), expected);
}

/// The crate-wide scan finds an alias declared with a comment between the
/// keyword and the name (`type /* c */ ApiResult<T> = ..`).
#[test]
fn crate_wide_lookup_reads_a_declaration_with_a_comment() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    std::fs::write(
        src.join("error.rs"),
        "pub type /* c */ ApiResult<T> = Result<Json<T>, AppError>;",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let mut sig: syn::Signature = syn::parse_str("fn h() -> ApiResult<User>").unwrap();
    assert!(expand_return_type_alias(&mut sig, None, None, Some(src)));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Result < Json < User > , AppError >"
    );
}
