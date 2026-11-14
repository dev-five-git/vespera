//! Generic sources of `schema!` / `schema_type!`: the struct's type
//! parameters are replaced by the type arguments the macro was given
//! (`Page<User>`), so the schema / DTO is built from the concrete fields.
//! Before, the arguments were ignored: `schema!(Page<User>)` documented an
//! unresolved `T`, and `schema_type!(Dto from Page<User>)` emitted fields of
//! a type `T` that did not exist at the call site.

use crate::parser::schema::substitute_type;

/// `item` with its type parameters replaced by the type arguments of `ty`'s
/// last path segment (a parameter's default fills a missing trailing
/// argument). `macro_name` prefixes the diagnostics, which are spanned at
/// `ty`.
pub fn instantiate(
    item: &syn::ItemStruct,
    ty: &syn::Type,
    macro_name: &str,
) -> syn::Result<syn::ItemStruct> {
    let name = &item.ident;
    let error = |message: String| syn::Error::new_spanned(ty, format!("{macro_name}: {message}"));
    if item
        .generics
        .params
        .iter()
        .any(|param| !matches!(param, syn::GenericParam::Type(_)))
    {
        return Err(error(format!(
            "`{name}` has lifetime or const parameters, which are not supported"
        )));
    }
    let args = type_arguments(ty)
        .map_err(|()| error(format!("only type arguments are supported for `{name}`")))?;
    let params: Vec<&syn::TypeParam> = item.generics.type_params().collect();
    if params.is_empty() && args.is_empty() {
        return Ok(item.clone());
    }
    let expected = || {
        params
            .iter()
            .map(|param| format!("`{}`", param.ident))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if args.len() > params.len() {
        return Err(error(format!(
            "`{name}` takes {} type argument(s) ({}), {} given",
            params.len(),
            expected(),
            args.len()
        )));
    }
    let mut concrete: Vec<&syn::Type> = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        match args
            .get(index)
            .copied()
            .or(param.default.as_ref().map(|(_, ty)| ty))
        {
            Some(arg) => concrete.push(arg),
            None => {
                return Err(error(format!(
                    "`{name}` is generic: give its type argument(s) ({}), e.g. `{name}<..>`",
                    expected()
                )));
            }
        }
    }
    let names: Vec<String> = params.iter().map(|param| param.ident.to_string()).collect();
    let mut instance = item.clone();
    instance.generics = syn::Generics::default();
    for field in &mut instance.fields {
        field.ty = substitute_type(&field.ty, &names, &concrete);
    }
    Ok(instance)
}

/// The type arguments of `ty`'s last path segment (`Err` for a lifetime /
/// const / binding argument).
fn type_arguments(ty: &syn::Type) -> Result<Vec<&syn::Type>, ()> {
    let syn::Type::Path(type_path) = ty else {
        return Ok(Vec::new());
    };
    let Some(syn::PathArguments::AngleBracketed(args)) = type_path
        .path
        .segments
        .last()
        .map(|segment| &segment.arguments)
    else {
        return Ok(Vec::new());
    };
    args.args
        .iter()
        .map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Ok(ty),
            _ => Err(()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use quote::ToTokens;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("pub struct S { pub a: i32 }", "S", "pub struct S { pub a : i32 }")]
    #[case::one_param(
        "pub struct Page<T> { pub items: Vec<T>, pub total: u64 }",
        "Page<User>",
        "pub struct Page { pub items : Vec < User > , pub total : u64 }"
    )]
    #[case::default_param(
        "pub struct Page<T, M = ()> { pub items: Vec<T>, pub meta: M }",
        "crate::Page<User>",
        "pub struct Page { pub items : Vec < User > , pub meta : () }"
    )]
    #[case::where_clause(
        "pub struct W<T> where T: Clone { pub value: Option<T> }",
        "W<String>",
        "pub struct W { pub value : Option < String > }"
    )]
    #[case::non_path_type("pub struct S { pub a: i32 }", "&S", "pub struct S { pub a : i32 }")]
    fn instantiates(#[case] item: &str, #[case] ty: &str, #[case] expected: &str) {
        let item: syn::ItemStruct = syn::parse_str(item).unwrap();
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let instance = instantiate(&item, &ty, "schema!").unwrap();
        assert_eq!(instance.to_token_stream().to_string(), expected);
    }

    #[rstest]
    #[case::missing_args(
        "pub struct Page<T> { pub items: Vec<T> }",
        "Page",
        "schema!: `Page` is generic: give its type argument(s) (`T`), e.g. `Page<..>`"
    )]
    #[case::too_many_args(
        "pub struct Page<T> { pub items: Vec<T> }",
        "Page<A, B>",
        "schema!: `Page` takes 1 type argument(s) (`T`), 2 given"
    )]
    #[case::not_generic(
        "pub struct S { pub a: i32 }",
        "S<A>",
        "schema!: `S` takes 0 type argument(s) (), 1 given"
    )]
    #[case::lifetime_param(
        "pub struct S<'a> { pub a: &'a str }",
        "S<'static>",
        "schema!: `S` has lifetime or const parameters, which are not supported"
    )]
    #[case::const_arg(
        "pub struct S<T> { pub a: T }",
        "S<3>",
        "schema!: only type arguments are supported for `S`"
    )]
    fn rejects(#[case] item: &str, #[case] ty: &str, #[case] expected: &str) {
        let item: syn::ItemStruct = syn::parse_str(item).unwrap();
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let err = instantiate(&item, &ty, "schema!").unwrap_err();
        assert_eq!(err.to_string(), expected);
    }
}
