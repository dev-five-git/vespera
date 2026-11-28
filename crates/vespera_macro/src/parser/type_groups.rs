//! Looking through the invisible wrappers a type can carry without changing
//! its meaning: `Type::Group` (a `$t:ty` fragment of a `macro_rules!`
//! expansion) and `Type::Paren` (`(T)`).
//!
//! Only these two are peeled. `Box` / `Arc` / `Rc` are real wrappers that
//! the callers classify on their own terms.

use syn::Type;

/// `ty` with every [`Type::Group`] / [`Type::Paren`] wrapper removed.
pub(super) fn strip_type_groups(mut ty: &Type) -> &Type {
    while let Type::Group(syn::TypeGroup { elem, .. }) | Type::Paren(syn::TypeParen { elem, .. }) =
        ty
    {
        ty = elem.as_ref();
    }
    ty
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use syn::Type;

    use super::strip_type_groups;

    #[rstest]
    #[case::plain("Option<String>", "Option < String >")]
    #[case::paren("(Option<String>)", "Option < String >")]
    #[case::nested("((u8))", "u8")]
    fn strips_parens_and_groups(#[case] written: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(written).unwrap();
        let stripped = strip_type_groups(&ty);
        assert_eq!(quote::quote!(#stripped).to_string(), expected);
    }

    #[test]
    fn strips_a_group_wrapper() {
        let inner: Type = syn::parse_str("Option<String>").unwrap();
        let grouped = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(inner),
        });
        let stripped = strip_type_groups(&grouped);
        assert_eq!(quote::quote!(#stripped).to_string(), "Option < String >");
    }
}
