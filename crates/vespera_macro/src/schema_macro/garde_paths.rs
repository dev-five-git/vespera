//! `#[garde(..)]` rules re-rooted at the source module: `schema_type!`
//! emits the DTO at its call site, where a `custom(check_name)` validator
//! or a container `context(Ctx)` type of the source module does not
//! resolve (E0425 / E0412). Paths the source module neither defines nor
//! imports are kept as written, like the re-rooted serde paths.

use quote::quote;
use syn::punctuated::Punctuated;

use super::transformation::reroot_path;

type Metas = Punctuated<syn::Meta, syn::Token![,]>;

/// A `garde(..)` meta (the attribute's, or one inside a `cfg_attr`) with its
/// `custom(..)` / `context(..)` paths re-rooted at `module_path`.
pub fn reroot_garde_meta(meta: &syn::Meta, module_path: &[String]) -> syn::Meta {
    match meta {
        syn::Meta::List(list) => list.parse_args_with(Metas::parse_terminated).map_or_else(
            |_| meta.clone(),
            |rules| {
                let path = &list.path;
                let rules = rules.iter().map(|rule| reroot_rule(rule, module_path));
                syn::parse_quote!(#path(#(#rules),*))
            },
        ),
        _ => meta.clone(),
    }
}

/// [`reroot_garde_meta`] for a whole `#[garde(..)]` attribute.
pub fn reroot_garde_attr(attr: &syn::Attribute, module_path: &[String]) -> syn::Attribute {
    let meta = reroot_garde_meta(&attr.meta, module_path);
    syn::parse_quote!(#[#meta])
}

/// One rule: `custom(f)` / `custom(f(args))` re-roots `f`, `context(Ctx)` /
/// `context(Ctx as ctx)` re-roots `Ctx`, any other list (`inner(..)`) is
/// walked; a list that holds no metas (`pattern(r"..")`) stays as written.
fn reroot_rule(rule: &syn::Meta, module_path: &[String]) -> syn::Meta {
    let syn::Meta::List(list) = rule else {
        return rule.clone();
    };
    let path = &list.path;
    if path.is_ident("custom") {
        return list.parse_args::<syn::Expr>().map_or_else(
            |_| rule.clone(),
            |mut expr| {
                reroot_callee(&mut expr, module_path);
                syn::parse_quote!(#path(#expr))
            },
        );
    }
    if path.is_ident("context") {
        return list.parse_args_with(parse_context).map_or_else(
            |_| rule.clone(),
            |(mut ty, binding)| {
                if let syn::Type::Path(type_path) = &mut ty
                    && type_path.qself.is_none()
                {
                    reroot_syn_path(&mut type_path.path, module_path);
                }
                let binding = binding.map(|name| quote!(as #name));
                syn::parse_quote!(#path(#ty #binding))
            },
        );
    }
    reroot_garde_meta(rule, module_path)
}

/// `Ctx` or `Ctx as name`.
fn parse_context(
    input: syn::parse::ParseStream<'_>,
) -> syn::Result<(syn::Type, Option<syn::Ident>)> {
    let ty: syn::Type = input.parse()?;
    let binding = if input.parse::<Option<syn::Token![as]>>()?.is_some() {
        Some(input.parse()?)
    } else {
        None
    };
    Ok((ty, binding))
}

/// The function a `custom(..)` expression names (`f`, or the callee of
/// `f(args)` building the validator).
fn reroot_callee(expr: &mut syn::Expr, module_path: &[String]) {
    match expr {
        syn::Expr::Path(path) if path.qself.is_none() => {
            reroot_syn_path(&mut path.path, module_path);
        }
        syn::Expr::Call(call) => reroot_callee(&mut call.func, module_path),
        _ => {}
    }
}

/// `path` resolved in `module_path`, when it has no generic arguments and
/// the module knows it.
fn reroot_syn_path(path: &mut syn::Path, module_path: &[String]) {
    if path
        .segments
        .iter()
        .any(|segment| !segment.arguments.is_none())
    {
        return;
    }
    let written = quote!(#path).to_string().replace(' ', "");
    if let Some(rerooted) = reroot_path(&written, module_path)
        .and_then(|rerooted| syn::parse_str::<syn::Path>(&rerooted).ok())
    {
        *path = rerooted;
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// `crate::models::user` defines `check_name` / `Ctx` and imports
    /// `crate::util::make_check`; `std_fn` is unknown there.
    fn garde_crate() -> (tempfile::TempDir, crate::env_access::test_env::EnvGuard) {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("models")).unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod models; pub mod util;").unwrap();
        std::fs::write(src.join("models.rs"), "pub mod user;").unwrap();
        std::fs::write(src.join("util.rs"), "pub fn make_check() {}").unwrap();
        std::fs::write(
            src.join("models").join("user.rs"),
            "use crate::util::make_check; fn check_name() {} pub struct Ctx;",
        )
        .unwrap();
        let guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        super::super::file_cache::bump_epoch();
        (dir, guard)
    }

    #[rstest]
    #[case::custom_fn(
        "#[garde(length(min = 1), custom(check_name))]",
        "# [garde (length (min = 1) , custom (crate :: models :: user :: check_name))]"
    )]
    #[case::custom_call(
        "#[garde(custom(make_check(&self.other)))]",
        "# [garde (custom (crate :: util :: make_check (& self . other)))]"
    )]
    #[case::nested_inner(
        "#[garde(inner(custom(check_name)))]",
        "# [garde (inner (custom (crate :: models :: user :: check_name)))]"
    )]
    #[case::context(
        "#[garde(context(Ctx))]",
        "# [garde (context (crate :: models :: user :: Ctx))]"
    )]
    #[case::context_binding(
        "#[garde(context(Ctx as ctx))]",
        "# [garde (context (crate :: models :: user :: Ctx as ctx))]"
    )]
    #[case::unknown_kept("#[garde(custom(std_fn))]", "# [garde (custom (std_fn))]")]
    #[case::closure_kept(
        "#[garde(custom(|v: &str, _| Ok(())))]",
        "# [garde (custom (| v : & str , _ | Ok (())))]"
    )]
    #[case::generic_kept("#[garde(context(Ctx<u8>))]", "# [garde (context (Ctx < u8 >))]")]
    #[case::non_meta_list_kept(
        r#"#[garde(pattern(r"^a"), skip)]"#,
        "# [garde (pattern (r\"^a\") , skip)]"
    )]
    #[case::unparseable_kept("#[garde(= 1)]", "# [garde (= 1)]")]
    #[case::bad_custom_kept("#[garde(custom(,))]", "# [garde (custom (,))]")]
    #[case::bad_context_kept("#[garde(context(Ctx as))]", "# [garde (context (Ctx as))]")]
    #[case::path_attr_kept("#[garde]", "# [garde]")]
    fn reroots_custom_and_context(#[case] attr: &str, #[case] expected: &str) {
        let (_dir, _guard) = garde_crate();
        let attr = syn::parse_str::<syn::DeriveInput>(&format!("{attr} struct S;"))
            .unwrap()
            .attrs
            .remove(0);
        let module = ["crate", "models", "user"].map(String::from);
        let rerooted = reroot_garde_attr(&attr, &module);
        assert_eq!(quote!(#rerooted).to_string(), expected);
    }
}
