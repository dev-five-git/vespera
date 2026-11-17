//! Generic type resolution and substitution for `OpenAPI` schema generation.
//!
//! This module handles the substitution of generic type parameters with concrete types
//! when generating schemas for generic structs like `Wrapper<T>`.

use syn::Type;

/// The path of the concrete type `segment` names when it is a generic
/// parameter (`T` with no arguments) and that type is a plain path.
fn concrete_param_path(
    segment: &syn::PathSegment,
    generic_params: &[String],
    concrete_types: &[&Type],
) -> Option<syn::Path> {
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }
    let index = generic_params
        .iter()
        .position(|param| segment.ident == *param)?;
    match concrete_types.get(index)? {
        Type::Path(concrete) if concrete.qself.is_none() => Some(concrete.path.clone()),
        _ => None,
    }
}

/// Substitutes generic type parameters with concrete types in a given type.
///
/// This function recursively walks through the type tree and replaces any
/// type parameters (like `T`, `U`) with their corresponding concrete types.
///
/// # Arguments
/// * `ty` - The type to transform
/// * `generic_params` - List of generic parameter names (e.g., `["T", "U"]`)
/// * `concrete_types` - List of concrete types to substitute (same order as params)
///
/// # Examples
/// Given `Vec<T>` with params `["T"]` and concrete types `[String]`,
/// returns `Vec<String>`.
// One arm per `syn::Type` shape; the path arm is kept whole with its generic
// argument, qualified-self and projection handling.
#[allow(clippy::too_many_lines)]
pub fn substitute_type(ty: &Type, generic_params: &[String], concrete_types: &[&Type]) -> Type {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.is_empty() {
                return ty.clone();
            }

            // Check if this is a direct generic parameter (e.g., just "T" with no arguments)
            if path.segments.len() == 1 {
                let segment = &path.segments[0];
                let ident_str = segment.ident.to_string();

                if matches!(&segment.arguments, syn::PathArguments::None) {
                    // Direct generic parameter substitution
                    if let Some(index) = generic_params.iter().position(|p| p == &ident_str)
                        && let Some(concrete_ty) = concrete_types.get(index)
                    {
                        return (*concrete_ty).clone();
                    }
                }
            }

            // For types with generic arguments (e.g., Vec<T>, Option<T>, HashMap<K, V>),
            // recursively substitute the type arguments. A qualified self type
            // (`<T as Tr>::Out`) and an associated-type binding (`Iterator<Item =
            // T>`) carry types too; a leading parameter of a projection (`T::Out`)
            // becomes the concrete type's path.
            let mut segments: Vec<syn::PathSegment> = path.segments.iter().cloned().collect();
            if path.segments.len() > 1
                && type_path.qself.is_none()
                && let Some(concrete_path) =
                    concrete_param_path(&path.segments[0], generic_params, concrete_types)
            {
                segments.splice(0..1, concrete_path.segments.iter().cloned());
            }
            let mut new_segments = syn::punctuated::Punctuated::new();
            for segment in &segments {
                let new_arguments = match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        let mut new_args = syn::punctuated::Punctuated::new();
                        for arg in &args.args {
                            let new_arg = match arg {
                                syn::GenericArgument::Type(inner_ty) => syn::GenericArgument::Type(
                                    substitute_type(inner_ty, generic_params, concrete_types),
                                ),
                                syn::GenericArgument::AssocType(binding) => {
                                    let mut binding = binding.clone();
                                    binding.ty = substitute_type(
                                        &binding.ty,
                                        generic_params,
                                        concrete_types,
                                    );
                                    syn::GenericArgument::AssocType(binding)
                                }
                                other => other.clone(),
                            };
                            new_args.push(new_arg);
                        }
                        syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
                            colon2_token: args.colon2_token,
                            lt_token: args.lt_token,
                            args: new_args,
                            gt_token: args.gt_token,
                        })
                    }
                    other => other.clone(),
                };

                new_segments.push(syn::PathSegment {
                    ident: segment.ident.clone(),
                    arguments: new_arguments,
                });
            }

            let mut new_type_path = type_path.clone();
            new_type_path.qself = type_path.qself.as_ref().map(|qself| syn::QSelf {
                ty: Box::new(substitute_type(&qself.ty, generic_params, concrete_types)),
                ..qself.clone()
            });
            new_type_path.path.segments = new_segments;
            Type::Path(new_type_path)
        }
        Type::Reference(type_ref) => {
            // Handle &T, &mut T
            let mut new_type_ref = type_ref.clone();
            new_type_ref.elem = Box::new(substitute_type(
                &type_ref.elem,
                generic_params,
                concrete_types,
            ));
            Type::Reference(new_type_ref)
        }
        Type::Slice(type_slice) => {
            // Handle [T]
            let mut new_type_slice = type_slice.clone();
            new_type_slice.elem = Box::new(substitute_type(
                &type_slice.elem,
                generic_params,
                concrete_types,
            ));
            Type::Slice(new_type_slice)
        }
        Type::Array(type_array) => {
            // Handle [T; N]
            let mut new_type_array = type_array.clone();
            new_type_array.elem = Box::new(substitute_type(
                &type_array.elem,
                generic_params,
                concrete_types,
            ));
            Type::Array(new_type_array)
        }
        Type::Tuple(type_tuple) => {
            // Handle (T1, T2, ...)
            let mut new_type_tuple = type_tuple.clone();
            new_type_tuple.elems = type_tuple
                .elems
                .iter()
                .map(|elem| substitute_type(elem, generic_params, concrete_types))
                .collect();
            Type::Tuple(new_type_tuple)
        }
        // `(T)` and a macro-expanded `$ty` (an invisible group) wrap one type.
        Type::Paren(paren) => Type::Paren(syn::TypeParen {
            elem: Box::new(substitute_type(&paren.elem, generic_params, concrete_types)),
            ..paren.clone()
        }),
        Type::Group(group) => Type::Group(syn::TypeGroup {
            elem: Box::new(substitute_type(&group.elem, generic_params, concrete_types)),
            ..group.clone()
        }),
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// A projection and an associated-type binding carry the parameter too:
    /// `<T as Tr>::Out`, `Source<Item = T>`, and `T::Out` (the concrete type's
    /// path leads it).
    #[rstest]
    #[case::qualified_self("<T as Tr>::Out", "<Foo as Tr>::Out")]
    #[case::assoc_binding("Source<Item = T>", "Source<Item = Foo>")]
    #[case::leading_param("T::Out", "Foo::Out")]
    #[case::unrelated_leading("Other::Out", "Other::Out")]
    fn substitutes_projections_and_bindings(#[case] ty: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(ty).unwrap();
        let foo: Type = syn::parse_str("Foo").unwrap();
        let substituted = substitute_type(&ty, &["T".to_string()], &[&foo]);
        let expected: Type = syn::parse_str(expected).unwrap();
        assert_eq!(
            quote::ToTokens::to_token_stream(&substituted).to_string(),
            quote::ToTokens::to_token_stream(&expected).to_string()
        );
    }

    #[rstest]
    #[case("$invalid", "String")]
    fn test_substitute_type_parse_failure_uses_original(
        #[case] invalid: &str,
        #[case] concrete_src: &str,
    ) {
        use std::str::FromStr;

        use proc_macro2::TokenStream;

        let ty = Type::Verbatim(TokenStream::from_str(invalid).unwrap());
        let concrete: Type = syn::parse_str(concrete_src).unwrap();
        let substituted = substitute_type(&ty, &[String::from("T")], &[&concrete]);
        assert_eq!(substituted, ty);
    }

    #[rstest]
    // Direct generic param substitution
    #[case("T", &["T"], &["String"], "String")]
    // Vec<T> substitution
    #[case("Vec<T>", &["T"], &["String"], "Vec < String >")]
    // Option<T> substitution
    #[case("Option<T>", &["T"], &["i32"], "Option < i32 >")]
    // Nested: Vec<Option<T>>
    #[case("Vec<Option<T>>", &["T"], &["String"], "Vec < Option < String > >")]
    // Deeply nested: Option<Vec<Option<T>>>
    #[case("Option<Vec<Option<T>>>", &["T"], &["bool"], "Option < Vec < Option < bool > > >")]
    // Multiple generic params
    #[case("HashMap<K, V>", &["K", "V"], &["String", "i32"], "HashMap < String , i32 >")]
    // Generic param not in list (unchanged)
    #[case("Vec<U>", &["T"], &["String"], "Vec < U >")]
    // Non-generic type (unchanged)
    #[case("String", &["T"], &["i32"], "String")]
    // Reference type: &T
    #[case("&T", &["T"], &["String"], "& String")]
    // Mutable reference: &mut T
    #[case("&mut T", &["T"], &["i32"], "& mut i32")]
    // Slice type: [T]
    #[case("[T]", &["T"], &["String"], "[String]")]
    // Array type: [T; 5]
    #[case("[T; 5]", &["T"], &["u8"], "[u8 ; 5]")]
    // Tuple type: (T, U)
    #[case("(T, U)", &["T", "U"], &["String", "i32"], "(String , i32)")]
    // Complex nested tuple
    #[case("(Vec<T>, Option<U>)", &["T", "U"], &["String", "bool"], "(Vec < String > , Option < bool >)")]
    // Parenthesized: (T) and Vec<(T)>
    #[case("(T)", &["T"], &["String"], "(String)")]
    #[case("Vec<(T)>", &["T"], &["String"], "Vec < (String) >")]
    // Reference to Vec<T>
    #[case("&Vec<T>", &["T"], &["String"], "& Vec < String >")]
    // Multi-segment path (no substitution for crate::Type)
    #[case("std::vec::Vec<T>", &["T"], &["String"], "std :: vec :: Vec < String >")]
    fn test_substitute_type_comprehensive(
        #[case] input: &str,
        #[case] params: &[&str],
        #[case] concrete: &[&str],
        #[case] expected: &str,
    ) {
        let ty: Type = syn::parse_str(input).unwrap();
        let generic_params: Vec<String> = params
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let concrete_types: Vec<Type> = concrete
            .iter()
            .map(|s| syn::parse_str(s).unwrap())
            .collect();
        let concrete_refs: Vec<&Type> = concrete_types.iter().collect();

        let result = substitute_type(&ty, &generic_params, &concrete_refs);
        let result_str = quote::quote!(#result).to_string();

        assert_eq!(result_str, expected, "Input: {input}");
    }

    /// A macro-expanded `$ty` reaches the walker as an invisible group.
    #[test]
    fn substitutes_inside_invisible_group() {
        let ty = Type::Group(syn::TypeGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            elem: Box::new(syn::parse_str("Vec<T>").unwrap()),
        });
        let concrete: Type = syn::parse_str("String").unwrap();
        let Type::Group(result) = substitute_type(&ty, &[String::from("T")], &[&concrete]) else {
            panic!("group preserved");
        };
        let elem = &result.elem;
        assert_eq!(quote::quote!(#elem).to_string(), "Vec < String >");
    }

    #[test]
    fn test_substitute_type_empty_path_segments() {
        // Create a TypePath with empty segments
        let ty = Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let concrete: Type = syn::parse_str("String").unwrap();
        let result = substitute_type(&ty, &[String::from("T")], &[&concrete]);
        // Should return the original type unchanged
        assert_eq!(result, ty);
    }

    #[test]
    fn matching_parameter_without_concrete_type_is_preserved() {
        let ty: Type = syn::parse_str("T").unwrap();
        let result = substitute_type(&ty, &[String::from("T")], &[]);

        assert_eq!(result, ty);
    }

    #[test]
    fn test_substitute_type_with_lifetime_generic_argument() {
        // Test type with lifetime: Cow<'static, T>
        // The lifetime argument should be preserved while T is substituted
        let ty: Type = syn::parse_str("std::borrow::Cow<'static, T>").unwrap();
        let concrete: Type = syn::parse_str("String").unwrap();
        let result = substitute_type(&ty, &[String::from("T")], &[&concrete]);
        let result_str = quote::quote!(#result).to_string();
        // Lifetime 'static should be preserved, T should be substituted
        assert_eq!(result_str, "std :: borrow :: Cow < 'static , String >");
    }

    #[test]
    fn test_substitute_type_parenthesized_args() {
        // A bare `fn(T) -> U` pointer is a `Type::BareFn`, not a path: it is
        // returned unchanged by the catch-all arm.
        let ty: Type = syn::parse_str("fn(T) -> U").unwrap();
        let concrete_t: Type = syn::parse_str("String").unwrap();
        let concrete_u: Type = syn::parse_str("i32").unwrap();
        let result = substitute_type(
            &ty,
            &[String::from("T"), String::from("U")],
            &[&concrete_t, &concrete_u],
        );
        // Type::BareFn doesn't go through the Path branch, falls to _ => ty.clone()
        assert_eq!(result, ty);
    }

    #[test]
    fn test_substitute_type_path_without_angle_brackets() {
        // Test path with parenthesized arguments: Fn(T) -> U as a trait
        let ty: Type = syn::parse_str("dyn Fn(T) -> U").unwrap();
        let concrete_t: Type = syn::parse_str("String").unwrap();
        let concrete_u: Type = syn::parse_str("i32").unwrap();
        let result = substitute_type(
            &ty,
            &[String::from("T"), String::from("U")],
            &[&concrete_t, &concrete_u],
        );
        // Type::TraitObject falls to _ => ty.clone()
        assert_eq!(result, ty);
    }

    #[test]
    fn test_substitute_type_path_with_parenthesized_arguments() {
        // A `Type::Path` whose segment carries `PathArguments::Parenthesized`
        // (`Fn(T) -> U`): the segment arguments go through the
        // `other => other.clone()` arm unchanged.
        let bound: syn::TraitBound = syn::parse_str("Fn(T) -> U").unwrap();
        assert!(matches!(
            bound.path.segments[0].arguments,
            syn::PathArguments::Parenthesized(_)
        ));
        let mut ty: Type = syn::parse_str("Placeholder").unwrap();
        if let Type::Path(path) = &mut ty {
            path.path = bound.path;
        }
        let concrete_t: Type = syn::parse_str("String").unwrap();
        let result = substitute_type(&ty, &[String::from("T")], &[&concrete_t]);
        assert_eq!(result, ty);
    }
}
