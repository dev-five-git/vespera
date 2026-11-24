//! Per-entry filter for the payload assertions a generic handler emits beside
//! itself. A `Validated<E<U>>` / `ValidatedWith<C, E<U>>` payload naming one of
//! the handler's own type, const or lifetime parameters is out of scope there
//! and skipped; every other payload (a concrete `Other`) is still asserted, so
//! a type without `#[derive(Schema)]` keeps its compile error.
//!
//! `responses` entries are not filtered: one naming a handler parameter is a
//! spanned error (`typed_response_assertions`, round 39), and every other entry
//! (a concrete `NotFound`) gets its schema assertion (round-40 MR3-L1).

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;

/// The generic parameters of a handler: every name (lifetimes by their bare
/// ident, which is how their `'a` shows up in a type) and the type parameters
/// alone. Only a type parameter can head a path (`T::Assoc`), so a lifetime or
/// const sharing a module's name (`'a` beside `a::NotFound`) names no type.
#[derive(Default)]
pub(super) struct Params {
    names: Vec<String>,
    types: Vec<String>,
}

impl Params {
    /// The parameters of `generics`.
    pub(super) fn of(generics: &syn::Generics) -> Self {
        let mut params = Self::default();
        for param in &generics.params {
            match param {
                syn::GenericParam::Lifetime(param) => {
                    params.names.push(param.lifetime.ident.to_string());
                }
                syn::GenericParam::Type(param) => {
                    let name = param.ident.to_string();
                    params.types.push(name.clone());
                    params.names.push(name);
                }
                syn::GenericParam::Const(param) => params.names.push(param.ident.to_string()),
            }
        }
        params
    }

    /// Whether `tokens` spells one of the parameters as a whole ident, at any
    /// group depth (`Vec<T>` mentions `T`, `Tree` does not mention `T`). An
    /// ident after `::` is a path segment, not the parameter (`api::Error` does
    /// not mention a generic `Error`). The head of a path (`T::Assoc`) still
    /// mentions a type parameter, but not a lifetime or const named like a
    /// module (`a::NotFound` under `<'a>` names the module `a`).
    pub(super) fn mentions(&self, tokens: &impl ToTokens) -> bool {
        fn walk(stream: TokenStream, params: &Params) -> bool {
            let trees: Vec<TokenTree> = stream.into_iter().collect();
            trees.iter().enumerate().any(|(index, tree)| match tree {
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    let qualified = index >= 2
                        && is_colon(trees.get(index - 2))
                        && is_colon(trees.get(index - 1));
                    let head = is_colon(trees.get(index + 1)) && is_colon(trees.get(index + 2));
                    !qualified
                        && params.names.contains(&name)
                        && (!head || params.types.contains(&name))
                }
                TokenTree::Group(group) => walk(group.stream(), params),
                TokenTree::Punct(_) | TokenTree::Literal(_) => false,
            })
        }
        walk(tokens.to_token_stream(), self)
    }
}

fn is_colon(tree: Option<&TokenTree>) -> bool {
    matches!(tree, Some(TokenTree::Punct(punct)) if punct.as_char() == ':')
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn params_cover_every_kind_of_generic_param() {
        let generics: syn::Generics = syn::parse_quote!(<'a, T, const N: usize>);
        let params = Params::of(&generics);
        assert_eq!(params.names, vec!["a", "T", "N"]);
        assert_eq!(params.types, vec!["T"]);
    }

    #[rstest]
    #[case::type_param("Vec<T>", &["T"], &["T"], true)]
    #[case::const_param("Wrap<N>", &["N"], &[], true)]
    #[case::lifetime_param("Foo<'a>", &["a"], &[], true)]
    #[case::nested_group("Option<(u8, Vec<T>)>", &["T"], &["T"], true)]
    #[case::concrete("NotFound", &["T"], &["T"], false)]
    #[case::longer_ident_is_not_a_match("Tree", &["T"], &["T"], false)]
    #[case::qualified_tail("api::T", &["T"], &["T"], false)]
    #[case::qualified_head("T::Assoc", &["T"], &["T"], true)]
    #[case::lifetime_is_not_a_path_head("a::NotFound", &["a"], &[], false)]
    #[case::const_is_not_a_path_head("N::Assoc", &["N"], &[], false)]
    #[case::no_params("Vec<u8>", &[], &[], false)]
    fn mentions_matches_whole_idents(
        #[case] ty: &str,
        #[case] names: &[&str],
        #[case] types: &[&str],
        #[case] expected: bool,
    ) {
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let params = Params {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            types: types.iter().map(|name| (*name).to_owned()).collect(),
        };
        assert_eq!(params.mentions(&ty), expected);
    }
}
