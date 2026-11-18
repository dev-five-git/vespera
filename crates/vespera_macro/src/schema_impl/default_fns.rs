//! Locate the function a `#[serde(default = "path")]` names, from the
//! deriving struct's own module.
//!
//! A bare name is looked up in the module that DEFINES the struct — the
//! inline `mod` it sits in, not the file's top level (where a same-named
//! function of another module would be picked, or none found).  A
//! `crate::a::b::f` path (what `schema_type!` re-roots a source field's
//! default function to) is looked up in that module's items; `self::` /
//! `super::` / `child::f` walk the inline modules of the struct's file, and
//! when that finds nothing (an out-of-line `mod defaults;`, `super::` past
//! the file's top level, a `use`-imported name) the path is resolved through
//! the crate's module tree from the struct's absolute module.

use std::path::Path;

use crate::parser::strip_raw_prefix_owned;
use crate::schema_macro::cfg_eval::statically_inactive;
use crate::schema_macro::{file_cache, module_scope};

fn ident_name(ident: &syn::Ident) -> String {
    strip_raw_prefix_owned(ident.to_string())
}

/// The literal value the default function `fn_path` returns, for the struct
/// named `struct_name` (its ident starting at `line`) in `file_path`.
pub(super) fn resolve_default_fn(
    fn_path: &str,
    struct_name: &str,
    line: usize,
    file_path: &Path,
) -> Option<serde_json::Value> {
    let path: syn::Path = syn::parse_str(fn_path).ok()?;
    if path.leading_colon.is_some() {
        return None;
    }
    let segments: Vec<String> = path.segments.iter().map(|s| ident_name(&s.ident)).collect();
    let (fn_name, module) = segments.split_last()?;
    if module.first().is_some_and(|first| first == "crate") {
        return crate_fn_value(module, fn_name);
    }
    // The struct's inline chain comes from the file's cached line index: the
    // file is parsed (per field) only when the function lives in an inline
    // module, not for the common top-level case.
    let chain = module_scope::item_module_chain_cached(file_path, struct_name, line)?;
    if let [owner] = module {
        // `Self::f` / `Type::f`: an associated function of an inherent
        // `impl` in the struct's module.
        let owner = if owner == "Self" { struct_name } else { owner };
        if let Some(value) = inherent_fn_value(&chain, owner, fn_name, file_path) {
            return Some(value);
        }
    }
    in_file(chain.clone(), module, fn_name, file_path)
        .or_else(|| in_module_tree(&segments, &chain, file_path))
}

/// The function `fn_name` of the crate module `module`, following `pub use`
/// re-exports (`pub use inner::role;`) when the module does not define it.
fn crate_fn_value(module: &[String], fn_name: &str) -> Option<serde_json::Value> {
    if let Some(value) =
        module_scope::module_items(module).and_then(|items| default_fn_value(&items, fn_name))
    {
        return Some(value);
    }
    if let Some(value) = crate::schema_macro::reexported_item(module, fn_name)
        .and_then(|(target, name)| default_fn_value(&module_scope::module_items(&target)?, &name))
    {
        return Some(value);
    }
    // `crate::m::Type::f` (what `schema_type!` makes of a source field's
    // `Self::f`): an associated function of an inherent `impl Type` in `m`.
    let (owner, parent) = module.split_last()?;
    associated_fn_value(&module_scope::module_items(parent)?, owner, fn_name)
}

/// The associated function `fn_name` of an inherent `impl owner` among the
/// items of the struct's module (the file's top level plus `chain`).
fn inherent_fn_value(
    chain: &[String],
    owner: &str,
    fn_name: &str,
    file_path: &Path,
) -> Option<serde_json::Value> {
    let items = inline_module_items(file_cache::get_parsed_file(file_path)?.items, chain)?;
    associated_fn_value(&items, owner, fn_name)
}

/// The associated function `fn_name` of an inherent `impl owner` among `items`.
fn associated_fn_value(
    items: &[syn::Item],
    owner: &str,
    fn_name: &str,
) -> Option<serde_json::Value> {
    items.iter().find_map(|item| match item {
        syn::Item::Impl(block)
            if block.trait_.is_none()
                && impl_owner_is(&block.self_ty, owner)
                && !statically_inactive(&block.attrs) =>
        {
            block.items.iter().find_map(|item| match item {
                syn::ImplItem::Fn(func)
                    if ident_name(&func.sig.ident) == fn_name
                        && !statically_inactive(&func.attrs) =>
                {
                    let func: syn::ItemFn =
                        syn::parse2(quote::ToTokens::to_token_stream(func)).ok()?;
                    crate::openapi_generator::extract_default_value_from_function(&func)
                }
                _ => None,
            })
        }
        _ => None,
    })
}

/// `self_ty` is the type `owner` (`impl Owner` / `impl<T> Owner<T>`).
fn impl_owner_is(self_ty: &syn::Type, owner: &str) -> bool {
    matches!(self_ty, syn::Type::Path(path) if path.qself.is_none()
        && path.path.segments.last().is_some_and(|s| ident_name(&s.ident) == owner))
}

/// The value a std default function returns for a field of type `ty`:
/// `Default::default` (`std::default::Default::default`, `<T>::default`,
/// `<T as Default>::default`), `T::default` and a std container's / `String`'s
/// `T::new`, where `T` is the field's type. `None` for any other path.
pub(super) fn std_type_default(fn_path: &str, ty: &syn::Type) -> Option<serde_json::Value> {
    let expr: syn::ExprPath = syn::parse_str(fn_path).ok()?;
    let segments: Vec<String> = expr
        .path
        .segments
        .iter()
        .map(|s| ident_name(&s.ident))
        .collect();
    let (function, owner) = segments.split_last()?;
    let is_default_trait = |path: &[String]| {
        matches!(path, [only] if only == "Default")
            || matches!(path, [root, module, name] if (root == "std" || root == "core")
                && module == "default" && name == "Default")
    };
    let last_name = |ty: &syn::Type| match ty {
        syn::Type::Path(path) => path.path.segments.last().map(|s| ident_name(&s.ident)),
        _ => None,
    };
    let field_ty = last_name(ty);
    let std_new = || {
        field_ty.as_deref().is_some_and(|name| {
            matches!(
                name,
                "String" | "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "HashMap" | "BTreeMap"
            )
        })
    };
    let applies = match &expr.qself {
        // `<T>::default` / `<T as Default>::default`, and `<T>::new` for a
        // std container `T` (the field's type).
        Some(qself) => {
            (function == "default"
                && (owner.is_empty() && qself.position == 0
                    || is_default_trait(owner) && qself.position == owner.len()))
                || (function == "new"
                    && owner.is_empty()
                    && qself.position == 0
                    && std_new()
                    && last_name(&qself.ty) == field_ty)
        }
        None if function == "default" && is_default_trait(owner) => true,
        None => {
            let constructor = match function.as_str() {
                "default" => true,
                "new" => std_new(),
                _ => false,
            };
            constructor && owner.last().is_some() && owner.last() == field_ty.as_ref()
        }
    };
    if applies {
        // Derive path: the struct (and its field types) sit at the call site.
        let module = crate::schema_macro::call_site_module().unwrap_or_default();
        crate::schema_macro::type_utils::get_type_default(ty, &module)
    } else {
        None
    }
}

/// `fn_name` through `module` (`self` / `super` / child segments) from the
/// inline `chain` of the struct's file, within that file only.
fn in_file(
    mut chain: Vec<String>,
    module: &[String],
    fn_name: &str,
    file_path: &Path,
) -> Option<serde_json::Value> {
    for segment in module {
        match segment.as_str() {
            "self" => {}
            "super" => {
                chain.pop()?;
            }
            child => chain.push(child.to_string()),
        }
    }
    if chain.is_empty() {
        // The file's top level: the per-file cache already holds it.
        return super::cached_default_functions(file_path)?
            .get(fn_name)
            .cloned();
    }
    let items = file_cache::get_parsed_file(file_path)?.items;
    default_fn_value(&inline_module_items(items, &chain)?, fn_name)
}

/// The function `segments` names from the struct's absolute module (the
/// file's module plus `chain`), through the crate's module tree: out-of-line
/// child modules, `super::` past the file and `use` imports included.
fn in_module_tree(
    segments: &[String],
    chain: &[String],
    file_path: &Path,
) -> Option<serde_json::Value> {
    let mut module = module_scope::file_module(file_path)?;
    module.extend(chain.iter().cloned());
    let module_scope::Resolved::Path(mut target) = module_scope::resolve_name(segments, &module)
    else {
        return None;
    };
    let fn_name = target.pop()?;
    if target.first().is_none_or(|root| root != "crate") {
        return None;
    }
    crate_fn_value(&target, &fn_name)
}

/// The literal value of the function `fn_name` among `items`.
fn default_fn_value(items: &[syn::Item], fn_name: &str) -> Option<serde_json::Value> {
    items.iter().find_map(|item| match item {
        syn::Item::Fn(func)
            if ident_name(&func.sig.ident) == fn_name && !statically_inactive(&func.attrs) =>
        {
            crate::openapi_generator::extract_default_value_from_function(func)
        }
        _ => None,
    })
}

/// The items of the inline module `chain` below `items`.
fn inline_module_items(items: Vec<syn::Item>, chain: &[String]) -> Option<Vec<syn::Item>> {
    chain.iter().try_fold(items, |items, segment| {
        items.into_iter().find_map(|item| match item {
            syn::Item::Mod(module) if ident_name(&module.ident) == *segment => {
                module.content.map(|(_, inner)| inner)
            }
            _ => None,
        })
    })
}
