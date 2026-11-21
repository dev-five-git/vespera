//! Generic type resolution and substitution for `OpenAPI` schema generation.
//!
//! This module handles the substitution of generic type parameters with concrete types
//! when generating schemas for generic structs like `Wrapper<T>`.

use std::cell::RefCell;

use quote::ToTokens;
use syn::{Type, punctuated::Punctuated};

/// The index of the generic parameter `segment` names: `T` with no arguments.
fn parameter_index(segment: &syn::PathSegment, generic_params: &[String]) -> Option<usize> {
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }
    generic_params
        .iter()
        .position(|param| segment.ident == *param)
}

/// A segment of a qualified path's trait (`<T as Trait<..>>::Out`) without its
/// associated-type bindings (`Item = u8`, `Item: Bound`): those belong to the
/// bound, and a qualified path rejects them (E0229).
fn without_bindings(segment: &syn::PathSegment) -> syn::PathSegment {
    let mut segment = segment.clone();
    if let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments {
        args.args = args
            .args
            .iter()
            .filter(|arg| {
                !matches!(
                    arg,
                    syn::GenericArgument::AssocType(_)
                        | syn::GenericArgument::AssocConst(_)
                        | syn::GenericArgument::Constraint(_)
                )
            })
            .cloned()
            .collect();
        if args.args.is_empty() {
            segment.arguments = syn::PathArguments::None;
        }
    }
    segment
}

/// Whether `ty` is the plain path `ident` (a type parameter's name).
fn is_parameter_type(ty: &Type, ident: &syn::Ident) -> bool {
    matches!(ty, Type::Path(type_path) if type_path.qself.is_none() && type_path.path.is_ident(ident))
}

/// The trait bounds each type parameter of `generics` carries: every non-`?`
/// trait bound, inline (`T: Clone + Deref`) or in a `where` clause (`where
/// T: Deref`), in that order. One entry per type parameter, in declaration
/// order; empty when it has no trait bound.
///
/// A projection of a parameter (`T::Target`) names its trait through these:
/// the substitution picks the bound that declares the projection and writes it
/// as `<Concrete as Deref>::Target`, the only form that names an associated
/// type of a concrete type.
pub fn parameter_bounds(generics: &syn::Generics) -> Vec<Vec<syn::Path>> {
    generics
        .type_params()
        .map(|param| {
            let clause_bounds = generics
                .where_clause
                .iter()
                .flat_map(|clause| clause.predicates.iter())
                .filter_map(|predicate| match predicate {
                    syn::WherePredicate::Type(predicate)
                        if is_parameter_type(&predicate.bounded_ty, &param.ident) =>
                    {
                        Some(predicate.bounds.iter())
                    }
                    _ => None,
                })
                .flatten();
            param
                .bounds
                .iter()
                .chain(clause_bounds)
                .filter_map(|bound| match bound {
                    syn::TypeParamBound::Trait(trait_bound) if trait_bound.maybe.is_none() => {
                        Some(trait_bound.path.clone())
                    }
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// The associated type names of the std traits a projection of a type
/// parameter is looked up in (`T::Target` with `Deref`). Traits not listed here
/// declare no projection this table knows.
const KNOWN_PROJECTIONS: &[(&str, &[&str])] = &[
    ("Deref", &["Target"]),
    ("DerefMut", &["Target"]),
    ("Iterator", &["Item"]),
    ("IntoIterator", &["Item", "IntoIter"]),
    ("Future", &["Output"]),
    ("IntoFuture", &["Output", "IntoFuture"]),
    ("FnOnce", &["Output"]),
    ("FnMut", &["Output"]),
    ("Fn", &["Output"]),
    ("ToOwned", &["Owned"]),
    ("FromStr", &["Err"]),
    ("TryFrom", &["Error"]),
    ("TryInto", &["Error"]),
    ("Index", &["Output"]),
    ("Add", &["Output"]),
    ("Sub", &["Output"]),
    ("Mul", &["Output"]),
    ("Div", &["Output"]),
    ("Rem", &["Output"]),
    ("Neg", &["Output"]),
    ("Not", &["Output"]),
    ("BitAnd", &["Output"]),
    ("BitOr", &["Output"]),
    ("BitXor", &["Output"]),
    ("Shl", &["Output"]),
    ("Shr", &["Output"]),
];

/// Whether `bound` is a std trait of [`KNOWN_PROJECTIONS`] declaring the
/// associated type `projection`.
fn declares_projection(bound: &syn::Path, projection: &syn::Ident) -> bool {
    bound.segments.last().is_some_and(|trait_segment| {
        KNOWN_PROJECTIONS.iter().any(|(name, projections)| {
            trait_segment.ident == *name && projections.iter().any(|p| projection == *p)
        })
    })
}

/// The bound a projection `param::projection` is qualified by, out of the
/// `bounds` of `param`: the only bound when there is one, else the only bound
/// declaring `projection` (see [`KNOWN_PROJECTIONS`]). `Ok(None)` without any
/// bound; an error when several bounds could declare it, asking for the
/// qualified form.
fn bound_of_projection<'a>(
    bounds: &'a [syn::Path],
    param: &syn::Ident,
    projection: &syn::Ident,
) -> syn::Result<Option<&'a syn::Path>> {
    match bounds {
        [] => Ok(None),
        [only] => Ok(Some(only)),
        _ => {
            let mut declaring = bounds
                .iter()
                .filter(|bound| declares_projection(bound, projection));
            let first = declaring.next();
            if let (Some(bound), None) = (first, declaring.next()) {
                return Ok(Some(bound));
            }
            let traits = bounds
                .iter()
                .map(|bound| bound.to_token_stream().to_string())
                .collect::<Vec<_>>()
                .join(" + ");
            Err(syn::Error::new(
                projection.span(),
                format!(
                    "`{param}::{projection}` is ambiguous: `{param}` is bounded by \
                     `{traits}`, so write the qualified form \
                     `<{param} as Trait>::{projection}` naming the trait"
                ),
            ))
        }
    }
}

/// One substitution: the parameters `params` replaced by `concrete` (same
/// order), with `bounds` (one entry per parameter, see [`parameter_bounds`])
/// resolving a projection of a parameter.
struct Subst<'a> {
    params: &'a [String],
    concrete: &'a [&'a Type],
    bounds: &'a [Vec<syn::Path>],
    /// The first projection no bound could be picked for (see
    /// [`bound_of_projection`]); the projection is left as written.
    ambiguous: RefCell<Option<syn::Error>>,
}

impl Subst<'_> {
    /// The trait a projection `param::projection` is qualified by.
    fn projection_trait(
        &self,
        index: usize,
        param: &syn::Ident,
        projection: &syn::Ident,
    ) -> Option<&syn::Path> {
        let bounds = self.bounds.get(index)?;
        match bound_of_projection(bounds, param, projection) {
            Ok(bound) => bound,
            Err(error) => {
                self.ambiguous.borrow_mut().get_or_insert(error);
                None
            }
        }
    }

    /// Walks one `syn::Type` shape by shape.
    fn ty(&self, ty: &Type) -> Type {
        match ty {
            Type::Path(type_path) => self.type_path(type_path),
            Type::Reference(type_ref) => {
                let mut new_type_ref = type_ref.clone();
                new_type_ref.elem = Box::new(self.ty(&type_ref.elem));
                Type::Reference(new_type_ref)
            }
            Type::Slice(type_slice) => {
                let mut new_type_slice = type_slice.clone();
                new_type_slice.elem = Box::new(self.ty(&type_slice.elem));
                Type::Slice(new_type_slice)
            }
            Type::Array(type_array) => {
                let mut new_type_array = type_array.clone();
                new_type_array.elem = Box::new(self.ty(&type_array.elem));
                Type::Array(new_type_array)
            }
            Type::Ptr(type_ptr) => {
                let mut new_type_ptr = type_ptr.clone();
                new_type_ptr.elem = Box::new(self.ty(&type_ptr.elem));
                Type::Ptr(new_type_ptr)
            }
            Type::Tuple(type_tuple) => {
                let mut new_type_tuple = type_tuple.clone();
                new_type_tuple.elems = type_tuple.elems.iter().map(|elem| self.ty(elem)).collect();
                Type::Tuple(new_type_tuple)
            }
            // `(T)` and a macro-expanded `$ty` (an invisible group) wrap one type.
            Type::Paren(paren) => Type::Paren(syn::TypeParen {
                elem: Box::new(self.ty(&paren.elem)),
                ..paren.clone()
            }),
            Type::Group(group) => Type::Group(syn::TypeGroup {
                elem: Box::new(self.ty(&group.elem)),
                ..group.clone()
            }),
            // `fn(T) -> U`
            Type::FnPtr(fn_ptr) => {
                let mut new_fn_ptr = fn_ptr.clone();
                new_fn_ptr.inputs = self.named_args(&fn_ptr.inputs);
                new_fn_ptr.output = self.return_type(&fn_ptr.output);
                Type::FnPtr(new_fn_ptr)
            }
            // `dyn Iterator<Item = T>`
            Type::TraitObject(trait_object) => {
                let mut new_object = trait_object.clone();
                new_object.bounds = self.bounds(&trait_object.bounds);
                Type::TraitObject(new_object)
            }
            // `impl Iterator<Item = T>`
            Type::ImplTrait(impl_trait) => {
                let mut new_impl = impl_trait.clone();
                new_impl.bounds = self.bounds(&impl_trait.bounds);
                Type::ImplTrait(new_impl)
            }
            _ => ty.clone(),
        }
    }

    /// `T` (a parameter with no arguments) becomes its concrete type; a
    /// projection of one (`T::Out`) is qualified by the parameter's trait; any
    /// other path has its generic arguments, qualified self and bindings
    /// walked.
    fn type_path(&self, type_path: &syn::TypePath) -> Type {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return Type::Path(type_path.clone());
        }
        if path.segments.len() == 1
            && let Some(concrete) = parameter_index(&path.segments[0], self.params)
                .and_then(|index| self.concrete.get(index))
        {
            return (*concrete).clone();
        }
        if path.segments.len() > 1
            && type_path.qself.is_none()
            && let Some(index) = parameter_index(&path.segments[0], self.params)
            && let Some(concrete) = self.concrete.get(index)
            && let Some(trait_path) =
                self.projection_trait(index, &path.segments[0].ident, &path.segments[1].ident)
        {
            // `<Concrete as Trait>::Out`: the trait's segments, then the
            // projection's own (`Out`, `Out::Inner`).
            let trait_path = self.path(trait_path);
            let mut segments: Punctuated<syn::PathSegment, syn::token::PathSep> =
                trait_path.segments.iter().map(without_bindings).collect();
            for segment in path.segments.iter().skip(1) {
                segments.push(self.segment(segment));
            }
            return Type::Path(syn::TypePath {
                qself: Some(syn::QSelf {
                    lt_token: syn::token::Lt::default(),
                    ty: Box::new(self.ty(concrete)),
                    position: trait_path.segments.len(),
                    as_token: Some(syn::token::As::default()),
                    gt_token: syn::token::Gt::default(),
                }),
                path: syn::Path {
                    leading_colon: trait_path.leading_colon,
                    segments,
                },
                ..type_path.clone()
            });
        }
        // A qualified self type (`<T as Tr>::Out`) carries a type too.
        let mut new_type_path = type_path.clone();
        new_type_path.qself = type_path.qself.as_ref().map(|qself| syn::QSelf {
            ty: Box::new(self.ty(&qself.ty)),
            ..qself.clone()
        });
        new_type_path.path = self.path(path);
        Type::Path(new_type_path)
    }

    fn path(&self, path: &syn::Path) -> syn::Path {
        syn::Path {
            leading_colon: path.leading_colon,
            segments: self.segments(&path.segments),
        }
    }

    fn segments<P: Default>(
        &self,
        segments: &Punctuated<syn::PathSegment, P>,
    ) -> Punctuated<syn::PathSegment, P> {
        segments
            .iter()
            .map(|segment| self.segment(segment))
            .collect()
    }

    fn segment(&self, segment: &syn::PathSegment) -> syn::PathSegment {
        syn::PathSegment {
            ident: segment.ident.clone(),
            arguments: self.arguments(&segment.arguments),
        }
    }

    /// The generic arguments of a path segment: `<..>` (types and
    /// associated-type bindings) or `Fn(..) -> ..`.
    fn arguments(&self, arguments: &syn::PathArguments) -> syn::PathArguments {
        match arguments {
            syn::PathArguments::None => syn::PathArguments::None,
            syn::PathArguments::AngleBracketed(args) => {
                let mut new_args = args.clone();
                new_args.args = args
                    .args
                    .iter()
                    .map(|arg| match arg {
                        syn::GenericArgument::Type(inner_ty) => {
                            syn::GenericArgument::Type(self.ty(inner_ty))
                        }
                        syn::GenericArgument::AssocType(binding) => {
                            let mut binding = binding.clone();
                            binding.ty = self.ty(&binding.ty);
                            syn::GenericArgument::AssocType(binding)
                        }
                        other => other.clone(),
                    })
                    .collect();
                syn::PathArguments::AngleBracketed(new_args)
            }
            syn::PathArguments::Parenthesized(args) => {
                let mut new_args = args.clone();
                new_args.inputs = self.named_args(&args.inputs);
                new_args.output = self.return_type(&args.output);
                syn::PathArguments::Parenthesized(new_args)
            }
        }
    }

    /// The parameters of a fn pointer / `Fn(..)` bound (`name: Type`).
    fn named_args<P: Default>(
        &self,
        args: &Punctuated<syn::NamedArg, P>,
    ) -> Punctuated<syn::NamedArg, P> {
        args.iter()
            .map(|arg| syn::NamedArg {
                ty: self.ty(&arg.ty),
                ..arg.clone()
            })
            .collect()
    }

    fn return_type(&self, output: &syn::ReturnType) -> syn::ReturnType {
        match output {
            syn::ReturnType::Default => syn::ReturnType::Default,
            syn::ReturnType::Type(arrow, ty) => {
                syn::ReturnType::Type(*arrow, Box::new(self.ty(ty)))
            }
        }
    }

    /// The trait bounds of a trait object / `impl Trait`: each trait's path
    /// arguments are walked (lifetime bounds have none).
    fn bounds<P: Default>(
        &self,
        bounds: &Punctuated<syn::TypeParamBound, P>,
    ) -> Punctuated<syn::TypeParamBound, P> {
        let mut new_bounds: Punctuated<syn::TypeParamBound, P> = bounds.iter().cloned().collect();
        for bound in &mut new_bounds {
            if let syn::TypeParamBound::Trait(trait_bound) = bound {
                trait_bound.path = self.path(&trait_bound.path);
            }
        }
        new_bounds
    }
}

/// Substitutes generic type parameters with concrete types in a given type.
///
/// This function recursively walks through the type tree and replaces any
/// type parameters (like `T`, `U`) with their corresponding concrete types.
///
/// A projection of a parameter (`T::Out`) is left as written: its trait is
/// unknown here. Use [`substitute_checked`] with [`parameter_bounds`] to
/// qualify it.
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
    substitute_checked(ty, generic_params, concrete_types, &[]).0
}

/// Substitution of `ty` with `bounds` (one list per parameter, from
/// [`parameter_bounds`]) qualifying each projection of a parameter by the
/// bound that declares it: `T::Target` with `T` = `Box<Foo>` and `Deref`
/// among its bounds becomes `<Box<Foo> as Deref>::Target`. A projection whose
/// parameter has no bound is left as written. Also returns the first
/// ambiguous projection's error (see [`bound_of_projection`]); an ambiguous
/// projection is left as written.
pub fn substitute_checked(
    ty: &Type,
    generic_params: &[String],
    concrete_types: &[&Type],
    bounds: &[Vec<syn::Path>],
) -> (Type, Option<syn::Error>) {
    let subst = Subst {
        params: generic_params,
        concrete: concrete_types,
        bounds,
        ambiguous: RefCell::new(None),
    };
    let ty = subst.ty(ty);
    (ty, subst.ambiguous.into_inner())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn tokens(ty: &impl quote::ToTokens) -> String {
        quote::ToTokens::to_token_stream(ty).to_string()
    }

    /// A projection and an associated-type binding carry the parameter too:
    /// `<T as Tr>::Out`, `Source<Item = T>`, and `T::Out` (the parameter's
    /// trait qualifies it, see [`parameter_bounds`]).
    #[rstest]
    #[case::qualified_self("<T as Tr>::Out", "Foo", None, "<Foo as Tr>::Out")]
    #[case::assoc_binding("Source<Item = T>", "Foo", None, "Source<Item = Foo>")]
    #[case::leading_param("T::Out", "Foo", Some("Tr"), "<Foo as Tr>::Out")]
    #[case::leading_param_boxed_concrete(
        "T::Target",
        "Box<Foo>",
        Some("std::ops::Deref"),
        "<Box<Foo> as std::ops::Deref>::Target"
    )]
    #[case::leading_param_nested_projection(
        "T::Out::Inner",
        "Foo",
        Some("Tr"),
        "<Foo as Tr>::Out::Inner"
    )]
    // No trait is known: the projection is left as written.
    #[case::leading_param_without_trait("T::Out", "Foo", None, "T::Out")]
    #[case::unrelated_leading("Other::Out", "Foo", Some("Tr"), "Other::Out")]
    // A leading parameter that carries arguments is not a parameter.
    #[case::leading_with_arguments("Wrap<u8>::Out", "Foo", Some("Tr"), "Wrap<u8>::Out")]
    // Any concrete type can be qualified (`<&Foo as Tr>::Out`).
    #[case::leading_reference_concrete("T::Out", "&Foo", Some("Tr"), "<&Foo as Tr>::Out")]
    // The bound's associated-type bindings stay on the bound, not on the
    // qualified trait (`<Vec<u8> as IntoIterator>::Item`, E0229 otherwise).
    #[case::leading_param_drops_binding(
        "T::Item",
        "Vec<u8>",
        Some("IntoIterator<Item = u8>"),
        "<Vec<u8> as IntoIterator>::Item"
    )]
    #[case::leading_param_keeps_trait_arguments(
        "T::Out",
        "Foo",
        Some("Tr<u8, Out: Clone>"),
        "<Foo as Tr<u8>>::Out"
    )]
    fn substitutes_projections_and_bindings(
        #[case] ty: &str,
        #[case] concrete: &str,
        #[case] trait_path: Option<&str>,
        #[case] expected: &str,
    ) {
        let ty: Type = syn::parse_str(ty).unwrap();
        let concrete: Type = syn::parse_str(concrete).unwrap();
        let bounds: Vec<Vec<syn::Path>> = vec![
            trait_path
                .map(|path| syn::parse_str(path).unwrap())
                .into_iter()
                .collect(),
        ];
        let (substituted, error) =
            substitute_checked(&ty, &["T".to_string()], &[&concrete], &bounds);
        assert!(error.is_none(), "{error:?}");
        let expected: Type = syn::parse_str(expected).unwrap();
        assert_eq!(tokens(&substituted), tokens(&expected));
    }

    #[test]
    fn parameter_bounds_reads_inline_and_where_bounds() {
        // A lifetime bound before the trait, a `?Sized` bound, and a where
        // predicate on a non-parameter type are skipped.
        let item: syn::ItemStruct = syn::parse_quote! {
            struct S<A: 'static + Iterator, B, C: ?Sized, D>
            where
                B: std::ops::Deref,
                Vec<A>: Clone,
                D: Clone + Send,
            {
                a: A,
            }
        };
        let bounds: Vec<Vec<String>> = parameter_bounds(&item.generics)
            .iter()
            .map(|bounds| bounds.iter().map(tokens).collect())
            .collect();
        assert_eq!(
            bounds,
            vec![
                vec!["Iterator".to_string()],
                vec!["std :: ops :: Deref".to_string()],
                vec![],
                vec!["Clone".to_string(), "Send".to_string()],
            ]
        );
    }

    #[test]
    fn parameter_bounds_keeps_every_trait_bound() {
        let item: syn::ItemStruct = syn::parse_quote! {
            struct S<T: 'static + Clone + Deref + ?Sized> where T: Iterator { a: T }
        };
        let bounds: Vec<Vec<String>> = parameter_bounds(&item.generics)
            .iter()
            .map(|bounds| bounds.iter().map(tokens).collect())
            .collect();
        assert_eq!(
            bounds,
            vec![vec![
                "Clone".to_string(),
                "Deref".to_string(),
                "Iterator".to_string(),
            ]]
        );
    }

    /// A projection with several bounds on its parameter is qualified by the
    /// one bound declaring it (`T::Target` by `Deref` next to `Clone`, `T::Item`
    /// by `Iterator` from a `where` clause).
    #[rstest]
    #[case::deref_next_to_clone(
        "struct Wrap<T: Clone + Deref> { x: T::Target }",
        "T::Target",
        "Box<Foo>",
        "<Box<Foo> as Deref>::Target"
    )]
    #[case::iterator_from_where_clause(
        "struct Wrap<T: Clone> where T: Iterator { x: T::Item }",
        "T::Item",
        "Vec<u8>",
        "<Vec<u8> as Iterator>::Item"
    )]
    #[case::second_bound_declares(
        "struct Wrap<T: Clone + Iterator + Send> { x: T::Item }",
        "T::Item",
        "Vec<u8>",
        "<Vec<u8> as Iterator>::Item"
    )]
    fn projection_is_qualified_by_the_bound_declaring_it(
        #[case] src: &str,
        #[case] projection: &str,
        #[case] concrete: &str,
        #[case] expected: &str,
    ) {
        let item: syn::ItemStruct = syn::parse_str(src).unwrap();
        let ty: Type = syn::parse_str(projection).unwrap();
        let concrete: Type = syn::parse_str(concrete).unwrap();
        let (substituted, error) = substitute_checked(
            &ty,
            &["T".to_string()],
            &[&concrete],
            &parameter_bounds(&item.generics),
        );
        assert!(error.is_none(), "{error:?}");
        let expected: Type = syn::parse_str(expected).unwrap();
        assert_eq!(tokens(&substituted), tokens(&expected));
    }

    /// A projection that no bound declares, or that several bounds could
    /// declare, is an error asking for the qualified form, spanned at the
    /// projection; the projection itself is not guessed at.
    #[rstest]
    #[case::no_bound_declares("struct Wrap<T: Clone + Deref> { x: T::Out }", "T::Out")]
    #[case::several_declare("struct Wrap<T: Iterator + IntoIterator> { x: T::Item }", "T::Item")]
    fn ambiguous_projection_asks_for_the_qualified_form(
        #[case] src: &str,
        #[case] projection: &str,
    ) {
        let item: syn::ItemStruct = syn::parse_str(src).unwrap();
        let ty: Type = syn::parse_str(projection).unwrap();
        let concrete: Type = syn::parse_str("Foo").unwrap();
        let (substituted, error) = substitute_checked(
            &ty,
            &["T".to_string()],
            &[&concrete],
            &parameter_bounds(&item.generics),
        );
        // The projection is left as written, and the error names the qualified form.
        assert_eq!(tokens(&substituted), tokens(&ty));
        let error = error.expect(projection).to_string();
        let name = projection.rsplit("::").next().unwrap();
        assert!(
            error.contains("ambiguous") && error.contains(&format!("<T as Trait>::{name}")),
            "{error}"
        );
    }

    #[rstest]
    #[case("$invalid", "String")]
    fn verbatim_type_passes_through_unchanged(#[case] invalid: &str, #[case] concrete_src: &str) {
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
    // Raw pointer: *const T
    #[case("*const T", &["T"], &["String"], "* const String")]
    // Bare function pointer: fn(T) -> U, and a `PhantomData` of one
    #[case("fn(T) -> U", &["T", "U"], &["String", "i32"], "fn (String) -> i32")]
    #[case("PhantomData<fn() -> T>", &["T"], &["String"], "PhantomData < fn () -> String >")]
    #[case("fn(T)", &["T"], &["String"], "fn (String)")]
    // A lifetime bound of a trait object is kept as written
    #[case("Box<dyn Iterator<Item = T> + 'static>", &["T"], &["String"], "Box < dyn Iterator < Item = String > + 'static >")]
    // Trait object and `impl Trait`: the bounds' arguments
    #[case("Box<dyn Iterator<Item = T>>", &["T"], &["String"], "Box < dyn Iterator < Item = String > >")]
    #[case("impl Iterator<Item = T>", &["T"], &["String"], "impl Iterator < Item = String >")]
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
        // A bare `fn(T) -> U` pointer: both its parameters and its return type
        // are substituted.
        let ty: Type = syn::parse_str("fn(T) -> U").unwrap();
        let concrete_t: Type = syn::parse_str("String").unwrap();
        let concrete_u: Type = syn::parse_str("i32").unwrap();
        let result = substitute_type(
            &ty,
            &[String::from("T"), String::from("U")],
            &[&concrete_t, &concrete_u],
        );
        assert_eq!(tokens(&result), "fn (String) -> i32");
    }

    #[test]
    fn test_substitute_type_path_without_angle_brackets() {
        // A trait object whose bound is `Fn(T) -> U`: the bound's parenthesized
        // arguments are substituted too.
        let ty: Type = syn::parse_str("dyn Fn(T) -> U").unwrap();
        let concrete_t: Type = syn::parse_str("String").unwrap();
        let concrete_u: Type = syn::parse_str("i32").unwrap();
        let result = substitute_type(
            &ty,
            &[String::from("T"), String::from("U")],
            &[&concrete_t, &concrete_u],
        );
        assert_eq!(tokens(&result), "dyn Fn (String) -> i32");
    }

    #[test]
    fn test_substitute_type_path_with_parenthesized_arguments() {
        // A `Type::Path` whose segment carries `PathArguments::Parenthesized`
        // (`Fn(T) -> U`): its inputs and output are walked.
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
        assert_eq!(tokens(&result), "Fn (String) -> U");
    }
}
