//! Expansion of type aliases used as a handler's return type.
//!
//! `type ApiResult<T> = Result<Json<T>, AppError>;` is a common way to spell
//! handler results, but the response parser only understands the shapes it
//! can see (`Result<..>`, `Json<..>`, ...). Before an operation is built the
//! return type is therefore expanded through the alias definitions vespera can
//! find:
//!
//! 1. a `type` item in the handler's own module (unless that module imports
//!    the name from another crate: `use axum::response::Result;`), then
//! 2. a single, unambiguous `type` item anywhere in the handler's file or
//!    under the crate's `src/` (two different definitions with the same name
//!    are left unexpanded).
//!
//! Test-only code (`#[cfg(test)]` modules, items and files) never defines a
//! handler's alias. Generic parameters (including defaults such as `type R<T,
//! E = AppError>`) are substituted, and an alias of an alias is followed a few
//! levels deep — resolved where the outer alias is defined, not where the
//! handler is. An alias that cannot be resolved is left untouched; the
//! response parser then documents it by description only instead of
//! inventing a schema.

use std::{borrow::Cow, path::Path};

use syn::{ReturnType, Type};

use crate::{
    collector::test_scope::{TestScope, attrs_test_only},
    parser::schema::is_builtin_openapi_type,
    schema_macro::file_cache,
};

/// How many alias-of-alias levels are followed before giving up.
const MAX_ALIAS_DEPTH: usize = 8;

/// Idents that are never user aliases (axum response wrappers handled by the
/// response parser). `Result` is special-cased: only a one-argument `Result<T>`
/// may be a user alias (`type Result<T> = std::result::Result<T, AppError>`).
const NON_ALIAS_IDENTS: [&str; 11] = [
    "Json",
    "Html",
    "Sse",
    "Bytes",
    "BytesMut",
    "Body",
    "Response",
    "Redirect",
    "ErrorResponse",
    "HeaderMap",
    "CookieJar",
];

/// Whether `sig`'s return type could name a user alias (so looking one up is
/// worth reading the source file).
pub fn needs_alias_expansion(sig: &syn::Signature) -> bool {
    matches!(&sig.output, ReturnType::Type(_, ty) if alias_candidate(ty).is_some())
}

/// Where a name is resolved: a parsed file and the inline module path of the
/// module inside it.
#[derive(Clone)]
struct Scope<'a> {
    file: Cow<'a, syn::File>,
    module: Vec<String>,
}

impl Scope<'_> {
    /// The module's own items (the file's top level when the path is gone).
    fn items(&self) -> &[syn::Item] {
        module_items(&self.file.items, &self.module).unwrap_or(&self.file.items)
    }
}

/// The items of the inline module at `path` below `items`.
fn module_items<'a>(items: &'a [syn::Item], path: &[String]) -> Option<&'a [syn::Item]> {
    let Some((first, rest)) = path.split_first() else {
        return Some(items);
    };
    items.iter().find_map(|item| match item {
        syn::Item::Mod(syn::ItemMod {
            ident,
            content: Some((_, inner)),
            ..
        }) if ident == first => module_items(inner, rest),
        _ => None,
    })
}

/// The inline module path of the first function `name` among `items`.
fn fn_module(items: &[syn::Item], name: &syn::Ident) -> Option<Vec<String>> {
    items.iter().find_map(|item| match item {
        syn::Item::Fn(function) if function.sig.ident == *name => Some(Vec::new()),
        syn::Item::Mod(syn::ItemMod {
            ident,
            content: Some((_, inner)),
            ..
        }) => fn_module(inner, name).map(|mut path| {
            path.insert(0, ident.to_string());
            path
        }),
        _ => None,
    })
}

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` (the handler's file) and (if given) the crate source
/// directory `src_dir`.
///
/// Returns `true` when the return type changed.
pub fn expand_return_type_alias(
    sig: &mut syn::Signature,
    same_file: Option<&syn::File>,
    src_dir: Option<&Path>,
) -> bool {
    let handler = sig.ident.clone();
    let ReturnType::Type(_, ty) = &mut sig.output else {
        return false;
    };
    let mut scope = same_file.map(|file| Scope {
        module: fn_module(&file.items, &handler).unwrap_or_default(),
        file: Cow::Borrowed(file),
    });
    let mut expanded = false;
    let mut seen: Vec<String> = Vec::new();
    while seen.len() < MAX_ALIAS_DEPTH {
        let Some(name) = alias_candidate(ty) else {
            break;
        };
        if seen.contains(&name)
            || matches!(&**ty, Type::Path(path) if is_external_path(&path.path, scope.as_ref()))
        {
            break;
        }
        let qualifier = alias_qualifier(ty);
        let Some((alias, alias_scope)) =
            find_alias(&name, qualifier.as_deref(), scope.as_ref(), src_dir)
        else {
            break;
        };
        let Some(next) = apply_alias(&alias, ty) else {
            break;
        };
        **ty = next;
        seen.push(name);
        expanded = true;
        // The alias body names types where the alias is defined.
        scope = Some(alias_scope);
    }
    expanded
}

/// The ident of `ty` if it may name a user alias.
fn alias_candidate(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() || type_path.path.leading_colon.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    let ident = segment.ident.to_string();
    if ident == "Result" {
        // `Result<T, E>` is std's; only a single-argument `Result<T>` can be a
        // user alias (axum's own `Result<T>` simply finds no alias).
        let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
            return None;
        };
        let type_args = args
            .args
            .iter()
            .filter(|arg| matches!(arg, syn::GenericArgument::Type(_)))
            .count();
        return (type_args == 1).then_some(ident);
    }
    if is_builtin_openapi_type(&ident) || NON_ALIAS_IDENTS.contains(&ident.as_str()) {
        return None;
    }
    Some(ident)
}

/// The module a qualified return type names its alias in (`v2` for
/// `v2::ApiResult<..>`, `crate::api::v2::ApiResult<..>`). `None` for a bare
/// name and for a `crate` / `self` / `super` qualifier, which are resolved
/// like a bare name.
fn alias_qualifier(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let mut segments = type_path.path.segments.iter().rev().skip(1);
    segments
        .next()
        .map(|module| &module.ident)
        .filter(|module| !is_local_root(module))
        .map(|module| syn::ext::IdentExt::unraw(module).to_string())
}

/// One `type <name>` definition: the alias, the scope it is defined in and
/// the innermost module that declares it (inline `mod`, else the file's
/// module; `None` when unknown).
type Definition<'a> = (syn::ItemType, Scope<'a>, Option<String>);

/// The `type <name>` definition visible from `scope`, if any, with the
/// scope it is defined in. A `qualifier` (`v2::ApiResult`) prefers the
/// definitions module `qualifier` declares and skips the handler's own
/// module.
fn find_alias<'a>(
    name: &str,
    qualifier: Option<&str>,
    scope: Option<&Scope<'a>>,
    src_dir: Option<&Path>,
) -> Option<(syn::ItemType, Scope<'a>)> {
    if let Some(scope) = scope.filter(|_| qualifier.is_none()) {
        let items = scope.items();
        if let Some(alias) = items.iter().find_map(|item| alias_item(item, name)) {
            return Some((alias, scope.clone()));
        }
        // `use axum::response::Result;` — the name is another crate's item,
        // so a same-named alias elsewhere in this crate is unrelated.
        if import_kind(items, name) == Some(Import::External) {
            return None;
        }
    }
    let mut definitions: Vec<Definition<'a>> = Vec::new();
    if let Some(scope) = scope {
        collect_aliases(&scope.file, None, name, &mut definitions);
    }
    if let Some(src_dir) = src_dir {
        let needle = format!("type {name}");
        let mut test_scope = TestScope::new(Some(src_dir));
        for path in file_cache::list_rs_files(src_dir).iter() {
            if let Some(file) = parsed_with(path, &needle)
                && !test_scope.file_test_only(path)
            {
                let module = file_module(path);
                collect_aliases(&Cow::Owned(file), module.as_deref(), name, &mut definitions);
            }
        }
    }
    if let Some(qualifier) = qualifier {
        let named: Vec<Definition<'a>> = definitions
            .iter()
            .filter(|(.., module)| module.as_deref() == Some(qualifier))
            .cloned()
            .collect();
        // Not declared in that module itself (a `pub use` re-export): any
        // unique definition.
        if !named.is_empty() {
            definitions = named;
        }
    }
    let mut definitions = definitions.into_iter();
    let (alias, alias_scope, _) = definitions.next()?;
    // The same alias seen twice (the handler's file is under `src/` too, a
    // re-declaration) is fine; two different definitions are ambiguous
    // without name resolution.
    definitions
        .all(|(other, ..)| same_definition(&alias, &other))
        .then_some((alias, alias_scope))
}

/// The module a source file declares: its stem, the directory name for a
/// `mod.rs`, nothing for the crate root.
fn file_module(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        "lib" | "main" => None,
        "mod" => file.parent()?.file_name()?.to_str().map(str::to_owned),
        _ => Some(stem.to_owned()),
    }
}

/// `item` when it is a non-test `type <name>` alias.
fn alias_item(item: &syn::Item, name: &str) -> Option<syn::ItemType> {
    match item {
        syn::Item::Type(alias) if alias.ident == name && !attrs_test_only(&alias.attrs) => {
            Some(alias.clone())
        }
        _ => None,
    }
}

/// Every non-test `type <name>` item of `file` (inline modules searched,
/// test-only modules skipped), each with the module it is defined in;
/// `file_module` is the module the file itself declares.
// The `Cow` itself is cloned into each scope (cheap when borrowed).
#[allow(clippy::ptr_arg)]
fn collect_aliases<'a>(
    file: &Cow<'a, syn::File>,
    file_module: Option<&str>,
    name: &str,
    out: &mut Vec<Definition<'a>>,
) {
    fn walk(
        items: &[syn::Item],
        name: &str,
        path: &mut Vec<String>,
        found: &mut Vec<(syn::ItemType, Vec<String>)>,
    ) {
        for item in items {
            if let Some(alias) = alias_item(item, name) {
                found.push((alias, path.clone()));
            } else if let syn::Item::Mod(syn::ItemMod {
                attrs,
                ident,
                content: Some((_, inner)),
                ..
            }) = item
                && !attrs_test_only(attrs)
            {
                path.push(ident.to_string());
                walk(inner, name, path, found);
                path.pop();
            }
        }
    }
    let mut found = Vec::new();
    walk(&file.items, name, &mut Vec::new(), &mut found);
    out.extend(found.into_iter().map(|(alias, module)| {
        let innermost = module
            .last()
            .cloned()
            .or_else(|| file_module.map(str::to_owned));
        (
            alias,
            Scope {
                file: file.clone(),
                module,
            },
            innermost,
        )
    }));
}

/// `crate` / `self` / `super`: a path root inside the current crate.
fn is_local_root(ident: &syn::Ident) -> bool {
    ident == "crate" || ident == "self" || ident == "super"
}

/// Whether the return type `ty` is a qualified path into another crate
/// (`axum::response::Result<..>`): never one of this crate's aliases. A
/// path whose first segment is `crate` / `self` / `super`, a module the
/// scope declares (`mod error;`) or a name it imports from this crate
/// (`use crate::error;`) stays a candidate; without the file, any other
/// qualified path counts as external.
fn is_external_path(path: &syn::Path, scope: Option<&Scope<'_>>) -> bool {
    let first = &path.segments[0].ident;
    path.segments.len() > 1
        && !is_local_root(first)
        && !scope.is_some_and(|scope| {
            let items = scope.items();
            declares_module(items, first)
                || import_kind(items, &first.to_string()) == Some(Import::Local)
        })
}

/// Whether `items` declare `mod <name>` (inline or file module).
fn declares_module(items: &[syn::Item], name: &syn::Ident) -> bool {
    items
        .iter()
        .any(|item| matches!(item, syn::Item::Mod(module) if module.ident == *name))
}

/// Where a `use` brings a name from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Import {
    /// `use crate::..` / `self::..` / `super::..`, or a module `items`
    /// declare (`use error::Result;` next to `mod error;`).
    Local,
    /// Any other root (`use axum::response::Result;`, `use ::std::..`).
    External,
}

/// The first `use` among the module's own `items` that brings `name` into
/// scope (`use a::Result;`, `use a::{B as Result};`). A `use` inside another
/// (inline) module does not reach this one. Without one, a glob of another
/// crate (`use axum::response::*;`) is the only way the name can be in
/// scope, unless a glob of this crate (`use crate::error::*;`) may bring it.
fn import_kind(items: &[syn::Item], name: &str) -> Option<Import> {
    let mut imports = Vec::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            let root = item_use.leading_colon.is_some().then_some(None);
            let local_module = |ident: &syn::Ident| declares_module(items, ident);
            use_tree_imports(&item_use.tree, root, &local_module, &mut imports);
        }
    }
    let named = imports
        .iter()
        .find(|(used, _)| used.as_deref() == Some(name));
    named.map(|(_, kind)| *kind).or_else(|| {
        let globs = || imports.iter().filter(|(used, _)| used.is_none());
        (globs().any(|(_, kind)| *kind == Import::External)
            && !globs().any(|(_, kind)| *kind == Import::Local))
        .then_some(Import::External)
    })
}

/// Every name `tree` brings into scope (`None` for a glob) with where it
/// comes from, in source order. `root` is `None` before the first path
/// segment, `Some(None)` after a leading `::` (always another crate),
/// `Some(Some(first))` otherwise. `local_module` tells whether a first
/// segment names a module of this crate.
#[allow(clippy::option_option)]
fn use_tree_imports(
    tree: &syn::UseTree,
    root: Option<Option<&syn::Ident>>,
    local_module: &dyn Fn(&syn::Ident) -> bool,
    out: &mut Vec<(Option<String>, Import)>,
) {
    let kind = || match root {
        Some(Some(first)) if is_local_root(first) || local_module(first) => Import::Local,
        // `use Name;` (root `None`) names an extern crate or a local item.
        None => Import::Local,
        Some(_) => Import::External,
    };
    match tree {
        syn::UseTree::Path(path) => use_tree_imports(
            &path.tree,
            root.or(Some(Some(&path.ident))),
            local_module,
            out,
        ),
        syn::UseTree::Name(used) => out.push((Some(used.ident.to_string()), kind())),
        syn::UseTree::Rename(rename) => out.push((Some(rename.rename.to_string()), kind())),
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                use_tree_imports(tree, root, local_module, out);
            }
        }
        syn::UseTree::Glob(_) => out.push((None, kind())),
    }
}

/// The parsed file at `path`. Files whose text lacks `needle` are skipped
/// without parsing.
fn parsed_with(path: &Path, needle: &str) -> Option<syn::File> {
    file_cache::get_file_content(path)
        .filter(|content| content.contains(needle))
        .and_then(|_| file_cache::get_parsed_file(path))
}

fn same_definition(a: &syn::ItemType, b: &syn::ItemType) -> bool {
    let tokens = |alias: &syn::ItemType| {
        let generics = &alias.generics;
        let ty = &alias.ty;
        quote::quote!(#generics = #ty).to_string()
    };
    tokens(a) == tokens(b)
}

/// `alias` applied to the use site `ty` (`ApiResult<User>` →
/// `Result<Json<User>, AppError>`), or `None` when the arguments do not fit
/// the alias's type parameters.
fn apply_alias(alias: &syn::ItemType, ty: &Type) -> Option<Type> {
    let segment = crate::schema_macro::type_utils::last_path_segment(ty)?;
    let args: Vec<&Type> = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
        // `None` (and the `Fn(..)` sugar, which never reaches a return type).
        _ => Vec::new(),
    };
    let params: Vec<&syn::TypeParam> = alias.generics.type_params().collect();
    if args.len() > params.len() {
        return None;
    }
    let names: Vec<String> = params.iter().map(|param| param.ident.to_string()).collect();
    let mut concrete: Vec<Type> = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        let ty = if let Some(arg) = args.get(index) {
            (*arg).clone()
        } else {
            // A default may name earlier parameters (`type R<T, E =
            // Wrapper<T>>`): they are already concrete.
            let earlier: Vec<&Type> = concrete.iter().collect();
            crate::parser::schema::substitute_type(
                &param.default.as_ref()?.1,
                &names[..index],
                &earlier,
            )
        };
        concrete.push(ty);
    }
    let concrete: Vec<&Type> = concrete.iter().collect();
    Some(crate::parser::schema::substitute_type(
        &alias.ty, &names, &concrete,
    ))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn expand(file_src: &str, ret: &str) -> String {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let mut sig: syn::Signature = syn::parse_str(&format!("fn handler() {ret}")).unwrap();
        expand_return_type_alias(&mut sig, Some(&file), None);
        let output = &sig.output;
        quote::quote!(#output).to_string()
    }

    #[rstest]
    #[case::generic_result_alias(
        "pub type ApiResult<T> = Result<Json<T>, AppError>;",
        "-> ApiResult<User>",
        "-> Result < Json < User > , AppError >"
    )]
    #[case::default_param(
        "type R<T, E = AppError> = Result<Json<T>, E>;",
        "-> R<User>",
        "-> Result < Json < User > , AppError >"
    )]
    #[case::default_names_earlier_param(
        "type R<T, E = Wrapper<T>> = Result<Json<T>, E>;",
        "-> R<User>",
        "-> Result < Json < User > , Wrapper < User > >"
    )]
    #[case::default_names_earlier_defaulted_param(
        "type R<T, U = Vec<T>, E = Wrapper<U>> = Result<Json<U>, E>;",
        "-> R<User>",
        "-> Result < Json < Vec < User > > , Wrapper < Vec < User > > >"
    )]
    #[case::user_result_alias(
        "type Result<T> = std::result::Result<T, AppError>;",
        "-> Result<Json<i32>>",
        "-> std :: result :: Result < Json < i32 > , AppError >"
    )]
    #[case::alias_of_alias(
        "type Inner<T> = Result<Json<T>, E>; type Outer = Inner<u8>;",
        "-> Outer",
        "-> Result < Json < u8 > , E >"
    )]
    #[case::alias_in_inline_module(
        "mod error { pub type ApiResult<T> = Result<T, AppError>; }",
        "-> ApiResult<Json<u8>>",
        "-> Result < Json < u8 > , AppError >"
    )]
    #[case::std_result_untouched(
        "type Result<T> = std::result::Result<T, AppError>;",
        "-> Result<Json<i32>, String>",
        "-> Result < Json < i32 > , String >"
    )]
    #[case::wrapper_untouched("type Json = Bad;", "-> Json<u8>", "-> Json < u8 >")]
    #[case::too_many_args("type R<T> = Result<T, E>;", "-> R<A, B>", "-> R < A , B >")]
    #[case::missing_arg_without_default("type R<T, E> = Result<T, E>;", "-> R<A>", "-> R < A >")]
    #[case::unknown_alias("", "-> ApiResult<User>", "-> ApiResult < User >")]
    #[case::self_recursive_stops(
        "type Result<T> = Result<T>;",
        "-> Result<u8>",
        "-> Result < u8 >"
    )]
    #[case::qualified_self("type F = u8;", "-> <X as Tr>::F", "-> < X as Tr > :: F")]
    #[case::non_path("type F = u8;", "-> (F, F)", "-> (F , F)")]
    #[case::result_without_args("type Result = u8;", "-> Result", "-> Result")]
    #[case::lifetime_arg_ignored(
        "type R<'a, T> = Result<Json<T>, E>;",
        "-> R<'static, u8>",
        "-> Result < Json < u8 > , E >"
    )]
    // Another crate's item is never expanded through a same-named alias.
    #[case::qualified_external(
        "type Result<T> = std::result::Result<T, AppError>;",
        "-> axum::response::Result<Json<u8>>",
        "-> axum :: response :: Result < Json < u8 > >"
    )]
    #[case::leading_colon(
        "type ApiResult<T> = Result<T, E>;",
        "-> ::app::ApiResult<u8>",
        "-> :: app :: ApiResult < u8 >"
    )]
    #[case::crate_path(
        "type ApiResult<T> = Result<T, E>;",
        "-> crate::ApiResult<u8>",
        "-> Result < u8 , E >"
    )]
    #[case::declared_module_path(
        "mod error { pub type ApiResult<T> = Result<T, E>; }",
        "-> error::ApiResult<u8>",
        "-> Result < u8 , E >"
    )]
    #[case::locally_imported_module_path(
        "use crate::errors; mod m { pub mod errors { pub type ApiResult<T> = Result<T, E>; } }",
        "-> errors::ApiResult<u8>",
        "-> Result < u8 , E >"
    )]
    // A qualified path names the alias of the module it names, never the
    // handler's own same-named alias.
    #[case::qualified_skips_handler_module(
        "type ApiResult<T> = Result<Json<T>, LocalErr>; mod v2 { pub type ApiResult<T> = Result<Json<T>, V2Err>; }",
        "-> v2::ApiResult<u8>",
        "-> Result < Json < u8 > , V2Err >"
    )]
    // Not declared in the named module (a re-export): the unique definition.
    #[case::qualified_reexport(
        "mod v2 { pub use super::inner::*; } mod inner { pub type ApiResult<T> = Result<T, E>; }",
        "-> v2::ApiResult<u8>",
        "-> Result < u8 , E >"
    )]
    // Test-only code never defines a handler's alias.
    #[case::cfg_test_module(
        "#[cfg(test)] mod tests { pub type ApiResult<T> = Result<T, TestError>; }",
        "-> ApiResult<u8>",
        "-> ApiResult < u8 >"
    )]
    #[case::cfg_test_item(
        "#[cfg(test)] type ApiResult<T> = Result<T, TestError>;",
        "-> ApiResult<u8>",
        "-> ApiResult < u8 >"
    )]
    // The handler's own module wins over another module's same-named alias.
    #[case::handler_module_scope(
        "mod b { type R = Json<i8>; } mod a { type R = Json<u8>; pub fn handler() {} }",
        "-> R",
        "-> Json < u8 >"
    )]
    // An external import is checked before another module's alias.
    #[case::import_before_other_module(
        "use axum::response::Result; mod inner { pub type Result<T> = std::result::Result<T, E>; }",
        "-> Result<Json<u8>>",
        "-> Result < Json < u8 > >"
    )]
    // Two different definitions in other modules are ambiguous.
    #[case::ambiguous_modules(
        "mod a { type R = Json<u8>; } mod b { type R = Json<i8>; }",
        "-> R",
        "-> R"
    )]
    fn expands_aliases(#[case] file_src: &str, #[case] ret: &str, #[case] expected: &str) {
        assert_eq!(expand(file_src, ret), expected);
    }

    #[test]
    fn qualified_path_without_file_is_external() {
        let mut sig: syn::Signature = syn::parse_str("fn h() -> error::ApiResult<u8>").unwrap();
        assert!(!expand_return_type_alias(&mut sig, None, None));
    }

    #[rstest]
    #[case::bare("ApiResult<u8>", None)]
    #[case::module("v2::ApiResult<u8>", Some("v2"))]
    #[case::innermost("crate::api::r#v2::ApiResult<u8>", Some("v2"))]
    #[case::local_root("crate::ApiResult<u8>", None)]
    #[case::non_path("(u8, u8)", None)]
    fn alias_qualifiers(#[case] ty: &str, #[case] expected: Option<&str>) {
        let ty: Type = syn::parse_str(ty).unwrap();
        assert_eq!(alias_qualifier(&ty).as_deref(), expected);
    }

    #[rstest]
    #[case::external("use axum::response::Result;", Some(Import::External))]
    #[case::local("use crate::error::Result;", Some(Import::Local))]
    #[case::self_root("use self::x::Result;", Some(Import::Local))]
    #[case::leading_colon("use ::std::result::Result;", Some(Import::External))]
    #[case::group_rename("use a::{b, c::D as Result};", Some(Import::External))]
    #[case::bare_name("use Result;", Some(Import::Local))]
    // Only another crate's glob can bring the name in.
    #[case::glob("use a::*;", Some(Import::External))]
    #[case::local_glob("use crate::error::*;", None)]
    #[case::external_and_local_globs("use axum::response::*; use crate::error::*;", None)]
    #[case::named_before_glob(
        "use axum::response::*; use crate::error::Result;",
        Some(Import::Local)
    )]
    #[case::other_name("use axum::response::Html;", None)]
    // A `use` inside another module does not reach this one.
    #[case::inline_module("mod m { use axum::Result; }", None)]
    #[case::declared_module("mod error; use error::Result;", Some(Import::Local))]
    #[case::module_without_body("mod m; struct S;", None)]
    fn import_kinds(#[case] file_src: &str, #[case] expected: Option<Import>) {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        assert_eq!(import_kind(&file.items, "Result"), expected);
    }

    #[rstest]
    // Only the module's own items: a nested module is another scope.
    #[case::nested("mod a { mod error {} }", false)]
    #[case::declared("mod error;", true)]
    #[case::other("mod a; struct error;", false)]
    fn module_declarations(#[case] file_src: &str, #[case] expected: bool) {
        let file: syn::File = syn::parse_str(file_src).unwrap();
        let name: syn::Ident = syn::parse_str("error").unwrap();
        assert_eq!(declares_module(&file.items, &name), expected);
    }

    /// `use axum::response::Result;` makes `Result<..>` axum's, so the
    /// crate's own `type Result<T>` elsewhere under `src/` does not apply.
    #[test]
    fn externally_imported_name_skips_crate_wide_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        std::fs::write(
            src.join("error.rs"),
            "pub type Result<T> = std::result::Result<T, AppError>;",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let handler_file: syn::File = syn::parse_str("use axum::response::Result;").unwrap();
        let mut sig: syn::Signature = syn::parse_str("fn h() -> Result<Json<u8>>").unwrap();
        assert!(!expand_return_type_alias(
            &mut sig,
            Some(&handler_file),
            Some(src)
        ));
        let local_file: syn::File = syn::parse_str("use crate::error::Result;").unwrap();
        assert!(expand_return_type_alias(
            &mut sig,
            Some(&local_file),
            Some(src)
        ));
    }
    /// An alias of an alias is resolved where the outer alias is defined
    /// (the handler's `use axum::response::Result` does not apply inside
    /// `error.rs`); `use error::Result` next to `mod error;` is this crate's
    /// `Result`; test-only files (`#[cfg(test)] mod fixtures;`) define
    /// nothing.
    #[test]
    fn crate_wide_aliases_resolve_in_their_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        std::fs::write(src.join("lib.rs"), "mod error; #[cfg(test)] mod fixtures;").unwrap();
        std::fs::write(
            src.join("error.rs"),
            "pub type Result<T> = std::result::Result<T, AppError>;\npub type ApiResult<T> = Result<Json<T>>;",
        )
        .unwrap();
        std::fs::write(
            src.join("fixtures.rs"),
            "pub type ApiResult<T> = std::result::Result<T, TestError>;",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let expand_in = |file_src: &str, ret: &str| {
            let file: syn::File = syn::parse_str(file_src).unwrap();
            let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
            expand_return_type_alias(&mut sig, Some(&file), Some(src));
            let output = &sig.output;
            quote::quote!(#output).to_string()
        };
        assert_eq!(
            expand_in("use axum::response::Result;", "-> ApiResult<u8>"),
            "-> std :: result :: Result < Json < u8 > , AppError >"
        );
        assert_eq!(
            expand_in("mod error; use error::Result;", "-> Result<u8>"),
            "-> std :: result :: Result < u8 , AppError >"
        );
    }

    /// `v2::ApiResult` is the alias `v2.rs` declares, not the handler
    /// module's; an external glob (`use axum::response::*;`) makes an
    /// unimported `Result` axum's, so the crate's `type Result<T>` does not
    /// apply.
    #[test]
    fn qualified_and_glob_imported_names_resolve_their_origin() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        std::fs::create_dir(src.join("api")).unwrap();
        std::fs::write(
            src.join("api").join("mod.rs"),
            "pub type ApiResult<T> = Result<Json<T>, ApiErr>;",
        )
        .unwrap();
        std::fs::write(
            src.join("v2.rs"),
            "pub type ApiResult<T> = Result<Json<T>, V2Err>;",
        )
        .unwrap();
        std::fs::write(
            src.join("error.rs"),
            "pub type Result<T> = std::result::Result<T, AppError>;",
        )
        .unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub type ApiResult<T> = Result<Json<T>, RootErr>;",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let expand_in = |file_src: &str, ret: &str| {
            let file: syn::File = syn::parse_str(file_src).unwrap();
            let mut sig: syn::Signature = syn::parse_str(&format!("fn h() {ret}")).unwrap();
            expand_return_type_alias(&mut sig, Some(&file), Some(src));
            let output = &sig.output;
            quote::quote!(#output).to_string()
        };
        let handler = "mod v2; mod api; type ApiResult<T> = Result<Json<T>, LocalErr>;";
        assert_eq!(
            expand_in(handler, "-> v2::ApiResult<u8>"),
            "-> Result < Json < u8 > , V2Err >"
        );
        assert_eq!(
            expand_in(handler, "-> api::ApiResult<u8>"),
            "-> Result < Json < u8 > , ApiErr >"
        );
        assert_eq!(
            expand_in("use axum::response::*;", "-> Result<Json<u8>>"),
            "-> Result < Json < u8 > >"
        );
        assert_eq!(
            expand_in(
                "use axum::response::*; use crate::error::*;",
                "-> Result<u8>"
            ),
            "-> std :: result :: Result < u8 , AppError >"
        );
    }

    #[test]
    fn default_return_is_untouched() {
        let mut sig: syn::Signature = syn::parse_str("fn handler()").unwrap();
        assert!(!expand_return_type_alias(&mut sig, None, None));
    }

    #[test]
    fn crate_wide_lookup_finds_unique_alias_and_rejects_ambiguity() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        std::fs::write(
            src.join("error.rs"),
            "pub type ApiResult<T> = Result<Json<T>, AppError>;\npub type Dup = A;",
        )
        .unwrap();
        std::fs::write(src.join("other.rs"), "pub type Dup = B; // type ApiResult").unwrap();
        std::fs::write(
            src.join("same.rs"),
            "pub type ApiResult<T> = Result<Json<T>, AppError>;",
        )
        .unwrap();
        std::fs::write(src.join("broken.rs"), "type Dup = ").unwrap();
        crate::schema_macro::file_cache::bump_epoch();

        let mut sig: syn::Signature = syn::parse_str("fn h() -> ApiResult<User>").unwrap();
        assert!(expand_return_type_alias(&mut sig, None, Some(src)));
        let output = &sig.output;
        assert_eq!(
            quote::quote!(#output).to_string(),
            "-> Result < Json < User > , AppError >"
        );

        let mut sig: syn::Signature = syn::parse_str("fn h() -> Dup").unwrap();
        assert!(!expand_return_type_alias(&mut sig, None, Some(src)));
    }
}
