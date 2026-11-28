//! Aliases written through `(T)` parentheses or a `$t:ty` group are expanded
//! like the bare alias: the response parser reads both as the type they wrap
//! (`type_groups`), so the alias layer must too (sidecar of `result_alias`;
//! `use super::*` semantics). The expansion keeps the written parentheses, which
//! the response parser removes later.

use rstest::rstest;

use super::*;

const USER_ALIASES: &str =
    "type ApiResult<T> = Result<Json<T>, AppError>; type UserList = Vec<User>;";

fn expand_return(file_src: &str, ret: &str) -> String {
    let file: syn::File = syn::parse_str(file_src).unwrap();
    let mut sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    expand_return_type_alias(&mut sig, None, Some(&file), None);
    let output = &sig.output;
    quote::quote!(#output).to_string()
}

#[rstest]
#[case::paren_return("-> (ApiResult<User>)", "-> (Result < Json < User > , AppError >)")]
#[case::paren_in_result(
    "-> Result<(ApiResult<User>), AppError>",
    "-> Result < (Result < Json < User > , AppError >) , AppError >"
)]
#[case::paren_json_body("-> Json<(UserList)>", "-> Json < (Vec < User >) >")]
fn expands_aliases_written_in_parentheses(#[case] ret: &str, #[case] expected: &str) {
    assert_eq!(expand_return(USER_ALIASES, ret), expected);
}

/// A `$t:ty` fragment (a `Type::Group`, which source text cannot spell) holding
/// the alias, as a `macro_rules!` expansion of `-> $ret` produces it.
#[test]
fn expands_an_alias_inside_a_group() {
    let file: syn::File = syn::parse_str(USER_ALIASES).unwrap();
    let written: Type = syn::parse_str("ApiResult<User>").unwrap();
    let grouped = Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(written),
    });
    let ret: syn::ReturnType = syn::parse_quote!(-> #grouped);
    let mut sig: syn::Signature = syn::parse_quote!(fn handler() #ret);
    assert!(expand_return_type_alias(&mut sig, None, Some(&file), None));
    let output = &sig.output;
    assert_eq!(
        quote::quote!(#output).to_string(),
        "-> Result < Json < User > , AppError >"
    );
}

#[rstest]
#[case::paren_return("-> (Created<User>)", true)]
#[case::paren_in_result("-> Result<(Created<User>), AppError>", true)]
#[case::paren_reference("-> (&'static str)", false)]
fn gate_looks_through_parentheses(#[case] ret: &str, #[case] expected: bool) {
    let sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    assert_eq!(needs_alias_expansion(&sig), expected);
}

#[test]
fn gate_looks_through_a_group() {
    let written: Type = syn::parse_str("Created<User>").unwrap();
    let grouped = Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(written),
    });
    let ret: syn::ReturnType = syn::parse_quote!(-> #grouped);
    let sig: syn::Signature = syn::parse_quote!(fn handler() #ret);
    assert!(needs_alias_expansion(&sig));
}
