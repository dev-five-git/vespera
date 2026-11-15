//! `#[garde(..)]` rules re-rooted at the source module: `schema_type!`
//! emits the DTO at its call site, where a `custom(check_name)` validator,
//! a container `context(Ctx)` type or a constant / static an expression
//! argument names (`length(max = MAX_NAME)`, `pattern(EMAIL_RE)`,
//! `if(cond = ..)`) of the source module does not resolve (E0425 / E0412). Paths the source module neither defines nor
//! imports are kept as written, like the re-rooted serde paths.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;

use super::transformation::reroot_path;

/// One garde rule: `name`, `name(..)` or `name = expr` (`if` is a keyword,
/// so the list is not parsed as `syn::Meta`).
enum Rule {
    Bare(syn::Ident),
    List(syn::Ident, TokenStream),
    Value(syn::Ident, syn::Expr),
}

impl syn::parse::Parse for Rule {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let name = syn::Ident::parse_any(input)?;
        if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            return Ok(Self::List(name, content.parse()?));
        }
        if input.parse::<Option<syn::Token![=]>>()?.is_some() {
            return Ok(Self::Value(name, input.parse()?));
        }
        Ok(Self::Bare(name))
    }
}

impl ToTokens for Rule {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self {
            Self::Bare(name) => quote!(#name),
            Self::List(name, args) => quote!(#name(#args)),
            Self::Value(name, value) => quote!(#name = #value),
        });
    }
}

type Rules = Punctuated<Rule, syn::Token![,]>;

/// A `garde(..)` meta (the attribute's, or one inside a `cfg_attr`) with the
/// source module's paths re-rooted at `module_path`.
pub fn reroot_garde_meta(meta: &syn::Meta, module_path: &[String]) -> syn::Meta {
    match meta {
        syn::Meta::List(list) => reroot_rules(list.tokens.clone(), module_path).map_or_else(
            || meta.clone(),
            |rules| {
                let path = &list.path;
                syn::parse_quote!(#path(#rules))
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

/// A rule list re-rooted (`None` when it is not one).
fn reroot_rules(tokens: TokenStream, module_path: &[String]) -> Option<TokenStream> {
    let rules = syn::parse::Parser::parse2(Rules::parse_terminated, tokens).ok()?;
    let rules = rules.into_iter().map(|rule| reroot_rule(rule, module_path));
    Some(quote!(#(#rules),*))
}

/// One rule: `custom(..)` / `pattern(..)` / `contains(..)` / `prefix(..)` /
/// `suffix(..)` re-root the paths of their expression, `context(Ctx)` /
/// `context(Ctx as ctx)` re-roots `Ctx`, a `key = expr` argument (`min =
/// MAX`, `cond = ..`) its expression, and any other list (`length(..)`,
/// `range(..)`, `inner(..)`, `if(..)`) is walked; a list that is none of
/// these stays as written.
fn reroot_rule(rule: Rule, module_path: &[String]) -> Rule {
    match rule {
        Rule::Value(name, mut value) => {
            reroot_expr(&mut value, module_path);
            Rule::Value(name, value)
        }
        Rule::List(name, args) => {
            let rerooted = match name.to_string().as_str() {
                "custom" | "pattern" | "contains" | "prefix" | "suffix" => {
                    syn::parse2::<syn::Expr>(args.clone()).ok().map(|mut expr| {
                        reroot_expr(&mut expr, module_path);
                        expr.into_token_stream()
                    })
                }
                "context" => syn::parse::Parser::parse2(parse_context, args.clone())
                    .ok()
                    .map(|(mut ty, binding)| {
                        if let syn::Type::Path(type_path) = &mut ty
                            && type_path.qself.is_none()
                        {
                            reroot_syn_path(&mut type_path.path, module_path);
                        }
                        let binding = binding.map(|name| quote!(as #name));
                        quote!(#ty #binding)
                    }),
                _ => reroot_rules(args.clone(), module_path),
            };
            Rule::List(name, rerooted.unwrap_or(args))
        }
        Rule::Bare(name) => Rule::Bare(name),
    }
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

/// The paths of `expr` resolved in `module_path` (`f` / `f(args)` of a
/// `custom(..)`, a `MAX` / `EMAIL_RE` constant), through calls, references
/// and operators; `self` / `Self::..` and closures stay as written.
fn reroot_expr(expr: &mut syn::Expr, module_path: &[String]) {
    match expr {
        syn::Expr::Path(path) if path.qself.is_none() && !names_self(&path.path) => {
            reroot_syn_path(&mut path.path, module_path);
        }
        syn::Expr::Call(call) => {
            reroot_expr(&mut call.func, module_path);
            call.args
                .iter_mut()
                .for_each(|arg| reroot_expr(arg, module_path));
        }
        syn::Expr::MethodCall(call) => {
            reroot_expr(&mut call.receiver, module_path);
            call.args
                .iter_mut()
                .for_each(|arg| reroot_expr(arg, module_path));
        }
        syn::Expr::Binary(binary) => {
            reroot_expr(&mut binary.left, module_path);
            reroot_expr(&mut binary.right, module_path);
        }
        syn::Expr::Reference(syn::ExprReference { expr, .. })
        | syn::Expr::Unary(syn::ExprUnary { expr, .. })
        | syn::Expr::Paren(syn::ExprParen { expr, .. }) => reroot_expr(expr, module_path),
        syn::Expr::Field(field) => reroot_expr(&mut field.base, module_path),
        _ => {}
    }
}

/// `self` (the validated value) or a `Self::..` path.
fn names_self(path: &syn::Path) -> bool {
    path.segments.first().is_some_and(|first| {
        first.ident == "Self" || (path.segments.len() == 1 && first.ident == "self")
    })
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
            "use crate::util::make_check; fn check_name() {} pub struct Ctx; \
             pub const MAX_NAME: usize = 50; static EMAIL_RE: u8 = 0;",
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
    #[case::length_const(
        "#[garde(length(min = 1, max = MAX_NAME))]",
        "# [garde (length (min = 1 , max = crate :: models :: user :: MAX_NAME))]"
    )]
    #[case::range_expr(
        "#[garde(range(max = MAX_NAME * 2 + (-MAX_NAME)))]",
        "# [garde (range (max = crate :: models :: user :: MAX_NAME * 2 + (- crate :: models :: user :: MAX_NAME)))]"
    )]
    #[case::pattern_static(
        "#[garde(pattern(EMAIL_RE))]",
        "# [garde (pattern (crate :: models :: user :: EMAIL_RE))]"
    )]
    #[case::pattern_deref(
        "#[garde(pattern(&*EMAIL_RE))]",
        "# [garde (pattern (& * crate :: models :: user :: EMAIL_RE))]"
    )]
    #[case::contains_method(
        "#[garde(contains(EMAIL_RE.to_string()), prefix(\"a\"), suffix(Ctx::x()))]",
        "# [garde (contains (crate :: models :: user :: EMAIL_RE . to_string ()) , prefix (\"a\") , suffix (crate :: models :: user :: Ctx :: x ()))]"
    )]
    #[case::if_cond(
        "#[garde(if(cond = self.n > MAX_NAME, length(max = MAX_NAME)))]",
        "# [garde (if (cond = self . n > crate :: models :: user :: MAX_NAME , length (max = crate :: models :: user :: MAX_NAME)))]"
    )]
    #[case::custom_const_arg(
        "#[garde(custom(make_check(MAX_NAME, Self::X, self)))]",
        "# [garde (custom (crate :: util :: make_check (crate :: models :: user :: MAX_NAME , Self :: X , self)))]"
    )]
    #[case::bad_pattern_kept("#[garde(pattern(,))]", "# [garde (pattern (,))]")]
    #[case::bad_list_kept("#[garde(length(min 1))]", "# [garde (length (min 1))]")]
    #[case::literal_kept(
        "#[garde(length(max = 5), dive, matches(other))]",
        "# [garde (length (max = 5) , dive , matches (other))]"
    )]
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
