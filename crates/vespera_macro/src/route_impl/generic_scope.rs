//! Per-entry filter for the schema and payload assertions a generic handler
//! emits beside itself. Only an entry naming one of the handler's own type,
//! const or lifetime parameters is out of scope there and skipped; every other
//! entry (a concrete `NotFound`, a payload `Other`) is still asserted, so a
//! typo or a type without `#[derive(Schema)]` keeps its compile error.

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;

/// The names of `generics`' lifetime, type and const parameters (a lifetime
/// by its bare ident, which is how its `'a` token shows up in a type).
pub(super) fn param_names(generics: &syn::Generics) -> Vec<String> {
    generics
        .params
        .iter()
        .map(|param| match param {
            syn::GenericParam::Lifetime(param) => param.lifetime.ident.to_string(),
            syn::GenericParam::Type(param) => param.ident.to_string(),
            syn::GenericParam::Const(param) => param.ident.to_string(),
        })
        .collect()
}

/// Whether `tokens` spells one of `names` as a whole ident, at any group depth
/// (`Vec<T>` mentions `T`, `Tree` does not mention `T`). An ident after `::`
/// is a path segment, not the parameter (`api::Error` does not mention a
/// generic `Error`), while the head of `T::Assoc` still does.
pub(super) fn mentions(tokens: &impl ToTokens, names: &[String]) -> bool {
    fn walk(stream: TokenStream, names: &[String]) -> bool {
        // Consecutive `:` puncts directly before the current token.
        let mut colons = 0_u8;
        stream.into_iter().any(|tree| {
            let path_segment = colons >= 2;
            colons = match &tree {
                TokenTree::Punct(punct) if punct.as_char() == ':' => colons.saturating_add(1),
                _ => 0,
            };
            match tree {
                TokenTree::Ident(ident) => {
                    !path_segment && names.iter().any(|name| ident == name.as_str())
                }
                TokenTree::Group(group) => walk(group.stream(), names),
                TokenTree::Punct(_) | TokenTree::Literal(_) => false,
            }
        })
    }
    walk(tokens.to_token_stream(), names)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn param_names_cover_every_kind_of_generic_param() {
        let generics: syn::Generics = syn::parse_quote!(<'a, T, const N: usize>);
        assert_eq!(param_names(&generics), vec!["a", "T", "N"]);
    }

    #[rstest]
    #[case::type_param("Vec<T>", &["T"], true)]
    #[case::const_param("Wrap<N>", &["N"], true)]
    #[case::lifetime_param("Foo<'a>", &["a"], true)]
    #[case::nested_group("Option<(u8, Vec<T>)>", &["T"], true)]
    #[case::concrete("NotFound", &["T"], false)]
    #[case::longer_ident_is_not_a_match("Tree", &["T"], false)]
    #[case::qualified_tail("api::T", &["T"], false)]
    #[case::qualified_head("T::Assoc", &["T"], true)]
    #[case::no_params("Vec<u8>", &[], false)]
    fn mentions_matches_whole_idents(
        #[case] ty: &str,
        #[case] names: &[&str],
        #[case] expected: bool,
    ) {
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(mentions(&ty, &names), expected);
    }
}
