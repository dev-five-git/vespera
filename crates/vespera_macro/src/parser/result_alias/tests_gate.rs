//! The alias gate (`needs_alias_expansion`) and the depth bound of nested
//! response positions (sidecar of `result_alias`; `use super::*` semantics).

use rstest::rstest;

use super::*;

/// An alias nested `wrappers` `Result` levels deep is expanded only while it
/// is inside the depth bound: `expand_alias` stops one level short of
/// `MAX_ALIAS_DEPTH` (`depth + 1 >= MAX_ALIAS_DEPTH`), so the last expanded
/// nesting is `MAX_ALIAS_DEPTH - 2`. Deeper positions are left as written.
#[rstest]
#[case::inside_bound(MAX_ALIAS_DEPTH - 2, true)]
#[case::at_bound(MAX_ALIAS_DEPTH - 1, false)]
#[case::past_bound(MAX_ALIAS_DEPTH, false)]
fn nesting_at_the_depth_bound(#[case] wrappers: usize, #[case] expanded: bool) {
    let file: syn::File = syn::parse_str("type Leaf<T> = Json<T>;").unwrap();
    let ret = format!(
        "-> {}Leaf<u8>{}",
        "Result<".repeat(wrappers),
        ", AppError>".repeat(wrappers)
    );
    let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
    assert_eq!(
        expand_return_type_alias(&mut sig, None, Some(&file), None),
        expanded
    );
}

/// The stored-signature gate (`needs_alias_expansion`) looks at the same
/// response positions as `expand_position`: an alias nested in a tuple or a
/// two-argument `Result` must open the source file too.
#[rstest]
#[case::top_level("-> Created<User>", true)]
#[case::result_ok("-> Result<Created<User>, AppError>", true)]
#[case::result_error("-> Result<u8, Created<User>>", true)]
#[case::tuple_element("-> (StatusCode, ApiResult<User>)", true)]
#[case::nested_tuple_in_result("-> Result<(StatusCode, Created<User>), AppError>", true)]
// A builtin-named position is a candidate too (a `type u8` in the handler's own
// file is what that name means), so the file is read for it.
#[case::plain_result("-> Result<u8, u16>", true)]
#[case::plain_tuple("-> (u8, String)", true)]
#[case::reference("-> &'static str", false)]
#[case::bare_result("-> Result", false)]
#[case::no_return("", false)]
fn needs_alias_expansion_follows_response_positions(#[case] ret: &str, #[case] expected: bool) {
    let sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
    assert_eq!(needs_alias_expansion(&sig), expected);
}
