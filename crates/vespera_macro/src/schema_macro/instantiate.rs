//! Generic sources of `schema!` / `schema_type!`: the struct's type
//! parameters are replaced by the type arguments the macro was given
//! (`Page<User>`), so the schema / DTO is built from the concrete fields.
//! Before, the arguments were ignored: `schema!(Page<User>)` documented an
//! unresolved `T`, and `schema_type!(Dto from Page<User>)` emitted fields of
//! a type `T` that did not exist at the call site.

use proc_macro2::TokenStream;
use quote::ToTokens;

use super::type_utils::resolve_type_with_params;
use crate::parser::schema::substitute_type;

/// One type parameter's value.
enum Arg {
    /// Written at the macro call site.
    CallSite(syn::Type),
    /// The parameter's default, written in the source struct's module (it
    /// may name earlier parameters: `P<T, M = Vec<T>>`).
    Default(syn::Type),
}

/// The type parameters of a generic source and the values the macro call
/// gives them.
pub struct Bindings {
    names: Vec<String>,
    args: Vec<Arg>,
}

impl Bindings {
    /// The parameters' values: call-site arguments as written, each default
    /// passed through `default` (with the earlier parameters' names) and
    /// then given the earlier parameters' values.
    fn values(&self, default: impl Fn(&syn::Type, &[String]) -> syn::Type) -> Vec<syn::Type> {
        let mut values: Vec<syn::Type> = Vec::with_capacity(self.args.len());
        for (index, arg) in self.args.iter().enumerate() {
            let earlier = &self.names[..index];
            values.push(match arg {
                Arg::CallSite(ty) => ty.clone(),
                Arg::Default(ty) => substitute(&default(ty, earlier), earlier, &values),
            });
        }
        values
    }

    /// The source field type `raw` (as written in the uninstantiated struct,
    /// in `source_module`) for the macro call site: its own names resolve in
    /// the source module, the call-site arguments are inserted unchanged
    /// (they are written at the call site, which may import names the source
    /// module does not), and defaults resolve in the source module.
    pub fn field_type(&self, raw: &syn::Type, source_module: &[String]) -> TokenStream {
        let values = self.values(|ty, earlier| resolve(ty, source_module, earlier));
        let resolved = resolve(raw, source_module, &self.names);
        substitute(&resolved, &self.names, &values).into_token_stream()
    }
}

/// What `Self` names on the instantiated source `name`: `Name` for a
/// non-generic one, else `Name::<A, ..>` with the parameters' values for the
/// macro call site (as [`Bindings::field_type`] inserts them).
pub fn self_path(
    name: &syn::Ident,
    bindings: Option<&Bindings>,
    source_module: &[String],
) -> syn::Path {
    bindings.map_or_else(
        || name.clone().into(),
        |bindings| {
            let values = bindings.values(|ty, earlier| resolve(ty, source_module, earlier));
            syn::parse_quote!(#name::<#(#values),*>)
        },
    )
}

/// `ty` resolved in `module`, the `params` left as they are.
fn resolve(ty: &syn::Type, module: &[String], params: &[String]) -> syn::Type {
    syn::parse2(resolve_type_with_params(ty, module, params)).unwrap_or_else(|_| ty.clone())
}

fn substitute(ty: &syn::Type, names: &[String], concrete: &[syn::Type]) -> syn::Type {
    let concrete: Vec<&syn::Type> = concrete.iter().collect();
    substitute_type(ty, names, &concrete)
}

/// `item` with its type parameters replaced by the type arguments of `ty`'s
/// last path segment (a parameter's default fills a missing trailing
/// argument). `macro_name` prefixes the diagnostics, which are spanned at
/// `ty`.
pub fn instantiate(
    item: &syn::ItemStruct,
    ty: &syn::Type,
    macro_name: &str,
) -> syn::Result<syn::ItemStruct> {
    instantiate_with_bindings(item, ty, macro_name).map(|(instance, _)| instance)
}

/// [`instantiate`], plus the bindings (`None` for a non-generic source).
pub fn instantiate_with_bindings(
    item: &syn::ItemStruct,
    ty: &syn::Type,
    macro_name: &str,
) -> syn::Result<(syn::ItemStruct, Option<Bindings>)> {
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
    let given = type_arguments(ty)
        .map_err(|()| error(format!("only type arguments are supported for `{name}`")))?;
    let params: Vec<&syn::TypeParam> = item.generics.type_params().collect();
    if params.is_empty() && given.is_empty() {
        return Ok((item.clone(), None));
    }
    let expected = || {
        params
            .iter()
            .map(|param| format!("`{}`", param.ident))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if given.len() > params.len() {
        return Err(error(format!(
            "`{name}` takes {} type argument(s) ({}), {} given",
            params.len(),
            expected(),
            given.len()
        )));
    }
    let mut args = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        args.push(match (given.get(index), &param.default) {
            (Some(arg), _) => Arg::CallSite((*arg).clone()),
            (None, Some((_, default))) => Arg::Default(default.clone()),
            (None, None) => {
                return Err(error(format!(
                    "`{name}` is generic: give its type argument(s) ({}), e.g. `{name}<..>`",
                    expected()
                )));
            }
        });
    }
    let bindings = Bindings {
        names: params.iter().map(|param| param.ident.to_string()).collect(),
        args,
    };
    let concrete = bindings.values(|ty, _| ty.clone());
    let mut instance = item.clone();
    instance.generics = syn::Generics::default();
    for field in &mut instance.fields {
        field.ty = substitute(&field.ty, &bindings.names, &concrete);
    }
    Ok((instance, Some(bindings)))
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
    // A default naming an earlier parameter gets that parameter's value.
    #[case::default_names_earlier_param(
        "pub struct P<T, M = Vec<T>> { pub item: T, pub meta: M }",
        "P<User>",
        "pub struct P { pub item : User , pub meta : Vec < User > }"
    )]
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

#[cfg(test)]
mod field_type_tests {
    use quote::ToTokens;
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    /// The source module `crate::page` defines `Meta` and imports `Item`
    /// from `models`; the call site passes its own `Row`.
    #[rstest]
    // The call-site argument is inserted as written (it used to be resolved
    // in the source module: `crate::page::Row`, which does not exist).
    #[case::call_site_arg("Vec<T>", "Page<Row>", "Vec < Row >")]
    // The source's own names still resolve in the source module.
    #[case::source_names("Option<Meta>", "Page<Row>", "Option < crate :: page :: Meta >")]
    // A default resolves in the source module, with the earlier argument
    // as written at the call site.
    #[case::default_resolved_in_source("M", "Page<Row>", "Vec < (Row , crate :: models :: Item) >")]
    #[case::default_overridden("M", "Page<Row, Other>", "Other")]
    fn field_type(#[case] raw: &str, #[case] source: &str, #[case] expected: &str) {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod page;\npub mod models;").unwrap();
        std::fs::write(src.join("models.rs"), "pub struct Item;").unwrap();
        std::fs::write(
            src.join("page.rs"),
            "use crate::models::Item;\npub struct Meta;\npub struct Page<T, M = Vec<(T, Item)>> { pub items: Vec<T>, pub meta: M }",
        )
        .unwrap();
        let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        crate::schema_macro::file_cache::bump_epoch();

        let item: syn::ItemStruct = syn::parse_str(
            "pub struct Page<T, M = Vec<(T, Item)>> { pub items: Vec<T>, pub meta: M }",
        )
        .unwrap();
        let ty: syn::Type = syn::parse_str(source).unwrap();
        let (_, bindings) = instantiate_with_bindings(&item, &ty, "schema_type!").unwrap();
        let raw: syn::Type = syn::parse_str(raw).unwrap();
        let module = ["crate", "page"].map(String::from);
        // `Self` names the instance (L5, round 16), the default resolved.
        let self_path = self_path(&item.ident, bindings.as_ref(), &module);
        assert!(
            quote::quote!(#self_path)
                .to_string()
                .starts_with("Page :: < Row ,"),
            "{}",
            quote::quote!(#self_path)
        );
        assert_eq!(
            bindings
                .unwrap()
                .field_type(&raw, &module)
                .to_token_stream()
                .to_string(),
            expected
        );
    }

    #[test]
    fn non_generic_source_has_no_bindings() {
        let item: syn::ItemStruct = syn::parse_str("pub struct S { pub a: i32 }").unwrap();
        let ty: syn::Type = syn::parse_str("S").unwrap();
        let (_, bindings) = instantiate_with_bindings(&item, &ty, "schema_type!").unwrap();
        assert!(bindings.is_none());
        let path = self_path(&item.ident, None, &[]);
        assert_eq!(quote::quote!(#path).to_string(), "S");
    }
}
