//! Expansion of type aliases used as a handler's return type.
//!
//! `type ApiResult<T> = Result<Json<T>, AppError>;` is a common way to spell
//! handler results, but the response parser only understands the shapes it
//! can see (`Result<..>`, `Json<..>`, ...). Before an operation is built the
//! return type is therefore expanded through the alias definitions vespera can
//! find:
//!
//! 1. a top-level `type` item in the handler's own file, then
//! 2. a single, unambiguous `type` item anywhere under the crate's `src/`
//!    (two different definitions with the same name are left unexpanded).
//!
//! Generic parameters (including defaults such as `type R<T, E = AppError>`)
//! are substituted, and an alias of an alias is followed a few levels deep.
//! An alias that cannot be resolved is left untouched; the response parser
//! then documents it by description only instead of inventing a schema.

use std::path::Path;

use syn::{ReturnType, Type};

use crate::{parser::schema::is_builtin_openapi_type, schema_macro::file_cache};

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

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` and (if given) the crate source directory `src_dir`.
///
/// Returns `true` when the return type changed.
pub fn expand_return_type_alias(
    sig: &mut syn::Signature,
    same_file: Option<&syn::File>,
    src_dir: Option<&Path>,
) -> bool {
    let ReturnType::Type(_, ty) = &mut sig.output else {
        return false;
    };
    let mut expanded = false;
    let mut seen: Vec<String> = Vec::new();
    while seen.len() < MAX_ALIAS_DEPTH {
        let Some(name) = alias_candidate(ty) else {
            break;
        };
        if seen.contains(&name) {
            break;
        }
        let Some(alias) = find_alias(&name, same_file, src_dir) else {
            break;
        };
        let Some(next) = apply_alias(&alias, ty) else {
            break;
        };
        **ty = next;
        seen.push(name);
        expanded = true;
    }
    expanded
}

/// The ident of `ty` if it may name a user alias.
fn alias_candidate(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
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

/// The `type <name>` definition visible for the handler, if any.
fn find_alias(
    name: &str,
    same_file: Option<&syn::File>,
    src_dir: Option<&Path>,
) -> Option<syn::ItemType> {
    if let Some(alias) = same_file.and_then(|file| alias_in_items(&file.items, name)) {
        return Some(alias);
    }
    let src_dir = src_dir?;
    let needle = format!("type {name}");
    let mut found: Option<syn::ItemType> = None;
    for path in file_cache::list_rs_files(src_dir).iter() {
        if !file_cache::get_file_content(path).is_some_and(|content| content.contains(&needle)) {
            continue;
        }
        let Some(file) = file_cache::get_parsed_file(path) else {
            continue;
        };
        let Some(alias) = alias_in_items(&file.items, name) else {
            continue;
        };
        // The same alias seen twice (e.g. a re-declaration) is fine; two
        // different definitions are ambiguous without name resolution.
        if found
            .as_ref()
            .is_some_and(|previous| !same_definition(previous, &alias))
        {
            return None;
        }
        found.get_or_insert(alias);
    }
    found
}

/// A `type <name>` item among `items`, searching inline modules too.
fn alias_in_items(items: &[syn::Item], name: &str) -> Option<syn::ItemType> {
    items.iter().find_map(|item| match item {
        syn::Item::Type(alias) if alias.ident == name => Some(alias.clone()),
        syn::Item::Mod(module) => module
            .content
            .as_ref()
            .and_then(|(_, items)| alias_in_items(items, name)),
        _ => None,
    })
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
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
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
    let mut concrete: Vec<&Type> = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        concrete.push(match args.get(index) {
            Some(arg) => arg,
            None => &param.default.as_ref()?.1,
        });
    }
    let names: Vec<String> = params.iter().map(|param| param.ident.to_string()).collect();
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
    fn expands_aliases(#[case] file_src: &str, #[case] ret: &str, #[case] expected: &str) {
        assert_eq!(expand(file_src, ret), expected);
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
