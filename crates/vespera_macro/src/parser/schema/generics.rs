//! Generic type resolution and substitution for `OpenAPI` schema generation.
//!
//! This module handles the substitution of generic type parameters with concrete types
//! when generating schemas for generic structs like `Wrapper<T>`.

use syn::Type;

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
            // recursively substitute the type arguments
            let mut new_segments = syn::punctuated::Punctuated::new();
            for segment in &path.segments {
                let new_arguments = match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        let mut new_args = syn::punctuated::Punctuated::new();
                        for arg in &args.args {
                            let new_arg = match arg {
                                syn::GenericArgument::Type(inner_ty) => syn::GenericArgument::Type(
                                    substitute_type(inner_ty, generic_params, concrete_types),
                                ),
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

            Type::Path(syn::TypePath {
                qself: type_path.qself.clone(),
                path: syn::Path {
                    leading_colon: path.leading_colon,
                    segments: new_segments,
                },
            })
        }
        Type::Reference(type_ref) => {
            // Handle &T, &mut T
            Type::Reference(syn::TypeReference {
                and_token: type_ref.and_token,
                lifetime: type_ref.lifetime.clone(),
                mutability: type_ref.mutability,
                elem: Box::new(substitute_type(
                    &type_ref.elem,
                    generic_params,
                    concrete_types,
                )),
            })
        }
        Type::Slice(type_slice) => {
            // Handle [T]
            Type::Slice(syn::TypeSlice {
                bracket_token: type_slice.bracket_token,
                elem: Box::new(substitute_type(
                    &type_slice.elem,
                    generic_params,
                    concrete_types,
                )),
            })
        }
        Type::Array(type_array) => {
            // Handle [T; N]
            Type::Array(syn::TypeArray {
                bracket_token: type_array.bracket_token,
                elem: Box::new(substitute_type(
                    &type_array.elem,
                    generic_params,
                    concrete_types,
                )),
                semi_token: type_array.semi_token,
                len: type_array.len.clone(),
            })
        }
        Type::Tuple(type_tuple) => {
            // Handle (T1, T2, ...)
            let new_elems = type_tuple
                .elems
                .iter()
                .map(|elem| substitute_type(elem, generic_params, concrete_types))
                .collect();
            Type::Tuple(syn::TypeTuple {
                paren_token: type_tuple.paren_token,
                elems: new_elems,
            })
        }
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

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

    #[test]
    fn test_substitute_type_empty_path_segments() {
        // Create a TypePath with empty segments
        let ty = Type::Path(syn::TypePath {
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
        // Fn(T) -> U style (parenthesized arguments)
        // This tests the `other => other.clone()` branch for PathArguments
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
}
