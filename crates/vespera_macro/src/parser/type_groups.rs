//! Looking through the invisible wrappers a type can carry without changing
//! its meaning: `Type::Group` (a `$t:ty` fragment of a `macro_rules!`
//! expansion) and `Type::Paren` (`(T)`).
//!
//! Only these two are peeled. `Box` / `Arc` / `Rc` are real wrappers that
//! the callers classify on their own terms.

use syn::{ReturnType, Type, visit_mut::VisitMut};

/// `ty` with every [`Type::Group`] / [`Type::Paren`] wrapper removed.
pub(super) fn strip_type_groups(mut ty: &Type) -> &Type {
    while let Type::Group(syn::TypeGroup { elem, .. }) | Type::Paren(syn::TypeParen { elem, .. }) =
        ty
    {
        ty = elem.as_ref();
    }
    ty
}

/// `ty` with the [`Type::Group`] / [`Type::Paren`] wrappers removed at every
/// depth (`Result<$t, E>` with `$t` a group is `Result<T, E>`): the shape a
/// parser classifies by, whatever macro_rules fragment or parentheses wrote it.
struct Ungroup;

impl VisitMut for Ungroup {
    fn visit_type_mut(&mut self, ty: &mut Type) {
        while let Type::Group(syn::TypeGroup { elem, .. })
        | Type::Paren(syn::TypeParen { elem, .. }) = ty
        {
            *ty = elem.as_ref().clone();
        }
        syn::visit_mut::visit_type_mut(self, ty);
    }
}

/// The return type with [`Ungroup`] applied (a copy; the input is untouched).
pub(super) fn ungroup_return(return_type: &ReturnType) -> ReturnType {
    let mut out = return_type.clone();
    Ungroup.visit_return_type_mut(&mut out);
    out
}

/// The `T` of an `Option<T>` written through groups (`ty` itself, groups
/// removed, when it is no `Option`): the optional field's or argument's type as
/// a parser reads it.
pub(super) fn unwrap_option(ty: &Type) -> &Type {
    let ty = strip_type_groups(ty);
    crate::schema_macro::type_utils::option_inner(ty).map_or(ty, strip_type_groups)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use syn::Type;

    use super::{strip_type_groups, unwrap_option};

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

    #[rstest]
    #[case::option("Option<(u8, u16)>", "(u8 , u16)")]
    #[case::option_in_paren("(Option<String>)", "String")]
    #[case::not_an_option("Vec<u8>", "Vec < u8 >")]
    fn unwraps_an_option_through_groups(#[case] written: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(written).unwrap();
        let inner = unwrap_option(&ty);
        assert_eq!(quote::quote!(#inner).to_string(), expected);
    }

    #[test]
    fn unwraps_an_option_inside_a_group() {
        let inner: Type = syn::parse_str("Option<u8>").unwrap();
        let grouped = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(inner),
        });
        let inner = unwrap_option(&grouped);
        assert_eq!(quote::quote!(#inner).to_string(), "u8");
    }

    /// `Result<Json<T>, String>` where the success type is a `$t:ty` group and
    /// a parenthesised error type, at every depth.
    #[test]
    fn ungroup_return_removes_groups_at_every_depth() {
        let json: Type = syn::parse_str("Json<u8>").unwrap();
        let grouped = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(json),
        });
        let ty: Type = syn::parse_quote!(Result<#grouped, (String)>);
        let written: syn::ReturnType = syn::parse_quote!(-> #ty);
        let ungrouped = super::ungroup_return(&written);
        let syn::ReturnType::Type(_, out) = &ungrouped else {
            panic!("a typed return");
        };
        assert_eq!(
            quote::quote!(#out).to_string(),
            "Result < Json < u8 > , String >"
        );
        // The input keeps its group (the rewrite works on a copy).
        let syn::ReturnType::Type(_, original) = &written else {
            panic!("a typed return");
        };
        let Type::Path(path) = original.as_ref() else {
            panic!("a path type");
        };
        let syn::PathArguments::AngleBracketed(args) =
            &path.path.segments.last().unwrap().arguments
        else {
            panic!("generic arguments");
        };
        assert!(matches!(
            args.args.first(),
            Some(syn::GenericArgument::Type(Type::Group(_)))
        ));
    }
}
