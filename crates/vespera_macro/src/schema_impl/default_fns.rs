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
use crate::schema_macro::cfg_eval::compiled_out;
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
        match inherent_fn(&chain, owner, fn_name, file_path) {
            Found::Unique(Some(value)) => return Some(value),
            Found::Ambiguous => return None,
            Found::Unique(None) | Found::Missing => {}
        }
    }
    match in_file(chain.clone(), module, fn_name, file_path) {
        Found::Unique(Some(value)) => Some(value),
        // A platform twin pair of the struct's own module: the module tree
        // would find the same pair again, so it is not searched (the search
        // is a whole-crate parse for a file the tree does not load).
        Found::Ambiguous => None,
        Found::Unique(None) | Found::Missing => in_module_tree(&segments, &chain, file_path),
    }
}

/// The function `fn_name` of the crate module `module`, following `pub use`
/// re-exports (`pub use inner::role;`) when the module does not define it.
fn crate_fn_value(module: &[String], fn_name: &str) -> Option<serde_json::Value> {
    if let Some(value) = module_scope::module_items(module)
        .and_then(|items| default_fn_value(&items, fn_name).into_value())
    {
        return Some(value);
    }
    if let Some(value) =
        crate::schema_macro::reexported_item(module, fn_name).and_then(|(target, name)| {
            default_fn_value(&module_scope::module_items(&target)?, &name).into_value()
        })
    {
        return Some(value);
    }
    // `crate::m::Type::f` (what `schema_type!` makes of a source field's
    // `Self::f`): an associated function of an inherent `impl Type` in `m`.
    let (owner, parent) = module.split_last()?;
    associated_fn(&module_scope::module_items(parent)?, owner, fn_name).into_value()
}

/// The associated function `fn_name` of an inherent `impl owner` among the
/// items of the struct's module (the file's top level plus `chain`).
fn inherent_fn(chain: &[String], owner: &str, fn_name: &str, file_path: &Path) -> Found {
    file_cache::get_parsed_file(file_path)
        .and_then(|file| inline_module_items(file.items, chain))
        .map_or(Found::Missing, |items| {
            associated_fn(&items, owner, fn_name)
        })
}

/// The associated function `fn_name` of an inherent `impl owner` among `items`.
fn associated_fn(items: &[syn::Item], owner: &str, fn_name: &str) -> Found {
    let candidates = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(block)
                if block.trait_.is_none()
                    && impl_owner_is(&block.self_ty, owner)
                    && !compiled_out(&block.attrs) =>
            {
                Some(block.items.iter().filter_map(|item| match item {
                    syn::ImplItem::Fn(func) if ident_name(&func.sig.ident) == fn_name => Some(func),
                    _ => None,
                }))
            }
            _ => None,
        })
        .flatten();
    active_candidate(candidates).map(|func| {
        syn::parse2::<syn::ItemFn>(quote::ToTokens::to_token_stream(func))
            .ok()
            .and_then(|func| crate::openapi_generator::extract_default_value_from_function(&func))
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
            // The qualified type must be the field's own type for `default`
            // too (`<Other>::default` on a `Vec<String>` field is `Other`'s
            // default, not the field's; R23-SM2).
            let same_type = last_name(&qself.ty) == field_ty;
            (function == "default"
                && same_type
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
fn in_file(mut chain: Vec<String>, module: &[String], fn_name: &str, file_path: &Path) -> Found {
    for segment in module {
        match segment.as_str() {
            "self" => {}
            "super" => {
                if chain.pop().is_none() {
                    return Found::Missing;
                }
            }
            child => chain.push(child.to_string()),
        }
    }
    if chain.is_empty() {
        // The file's top level: the per-file cache holds the unique literal
        // defaults; any other name is classified from the parsed file.
        if let Some(value) = super::cached_default_functions(file_path)
            .and_then(|values| values.get(fn_name).cloned())
        {
            return Found::Unique(Some(value));
        }
    }
    let Some(items) = file_cache::get_parsed_file(file_path)
        .and_then(|file| inline_module_items(file.items, &chain))
    else {
        return Found::Missing;
    };
    default_fn_value(&items, fn_name)
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

/// The literal value of the function `fn_name` among `items`. Several active
/// items of that name (a `#[cfg(unix)]` / `#[cfg(windows)]` pair the static
/// cfg check cannot decide) leave it unresolved: see [`unique_active`].
fn default_fn_value(items: &[syn::Item], fn_name: &str) -> Found {
    active_candidate(items.iter().filter_map(|item| match item {
        syn::Item::Fn(func) if ident_name(&func.sig.ident) == fn_name => Some(func),
        _ => None,
    }))
    .map(crate::openapi_generator::extract_default_value_from_function)
}

/// What a lookup of a function name in one item list found. `Ambiguous` is a
/// platform twin pair the static cfg check cannot decide: it resolves to
/// nothing, and the name is not searched for elsewhere.
enum Found<T = Option<serde_json::Value>> {
    Unique(T),
    Missing,
    Ambiguous,
}

impl Found {
    /// The literal value of a unique function (`None` when there is none).
    fn into_value(self) -> Option<serde_json::Value> {
        match self {
            Self::Unique(value) => value,
            Self::Missing | Self::Ambiguous => None,
        }
    }
}

impl<'a, T: HasAttrs + 'a> Found<&'a T> {
    fn map(self, extract: impl FnOnce(&'a T) -> Option<serde_json::Value>) -> Found {
        match self {
            Found::Unique(item) => Found::Unique(extract(item)),
            Found::Missing => Found::Missing,
            Found::Ambiguous => Found::Ambiguous,
        }
    }
}

/// The one candidate of `candidates` that is not statically inactive, or
/// `None` when there is not exactly one. The top-level, inline-module,
/// `crate::` and associated lookups all select through this, so a platform
/// twin pair resolves the same way (to nothing) wherever the default is read
/// (R23-SM3: the top level took the last twin, an inline module the first).
pub(super) fn unique_active<'a, T: HasAttrs + 'a>(
    candidates: impl Iterator<Item = &'a T>,
) -> Option<&'a T> {
    match active_candidate(candidates) {
        Found::Unique(only) => Some(only),
        Found::Missing | Found::Ambiguous => None,
    }
}

/// [`unique_active`] that tells "no active candidate" from "a twin pair".
fn active_candidate<'a, T: HasAttrs + 'a>(candidates: impl Iterator<Item = &'a T>) -> Found<&'a T> {
    let mut active = candidates.filter(|candidate| !compiled_out(candidate.attrs()));
    match (active.next(), active.next()) {
        (None, _) => Found::Missing,
        (Some(only), None) => Found::Unique(only),
        (Some(_), Some(_)) => Found::Ambiguous,
    }
}

/// The attributes of an item-like node, for [`unique_active`].
pub(super) trait HasAttrs {
    fn attrs(&self) -> &[syn::Attribute];
}

impl HasAttrs for syn::ItemFn {
    fn attrs(&self) -> &[syn::Attribute] {
        &self.attrs
    }
}

impl HasAttrs for syn::ImplItemFn {
    fn attrs(&self) -> &[syn::Attribute] {
        &self.attrs
    }
}

impl HasAttrs for syn::ItemMod {
    fn attrs(&self) -> &[syn::Attribute] {
        &self.attrs
    }
}

/// The items of the inline module `chain` below `items`. A `#[cfg]`-disabled
/// twin module of the same name is skipped, as the module chain is (R25-SM0-M1):
/// the struct is resolved in the active twin, so its function must be too.
pub(super) fn inline_module_items(
    items: Vec<syn::Item>,
    chain: &[String],
) -> Option<Vec<syn::Item>> {
    // The same `unique_active` rule as the other lookups: two active twins of
    // one module name (`#[cfg(unix)] mod inner` / `#[cfg(windows)] mod inner`)
    // resolve to nothing, not to whichever twin comes first (R27-SM0-L1).
    chain.iter().try_fold(items, |items, segment| {
        let module = unique_active(items.iter().filter_map(|item| match item {
            syn::Item::Mod(module) if ident_name(&module.ident) == *segment => Some(module),
            _ => None,
        }))?;
        module.content.as_ref().map(|(_, inner)| inner.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(source: &str) -> Vec<syn::Item> {
        syn::parse_file(source).expect("valid source").items
    }

    #[test]
    fn one_active_inline_module_resolves() {
        let items = items("mod inner { pub fn f() {} }");
        let inner = inline_module_items(items, &["inner".to_string()]);
        assert_eq!(inner.map(|inner| inner.len()), Some(1));
    }

    /// R27-SM0-L1: a platform twin pair of an inline module resolves to
    /// nothing, as the top-level and `crate::` lookups do, not to the first twin.
    #[test]
    fn two_active_inline_module_twins_resolve_to_nothing() {
        let items = items(
            "#[cfg(unix)] mod inner { pub fn f() {} } #[cfg(windows)] mod inner { pub fn g() {} }",
        );
        assert!(inline_module_items(items, &["inner".to_string()]).is_none());
    }

    #[test]
    fn a_disabled_inline_module_twin_is_skipped() {
        let items = items("#[cfg(any())] mod inner { pub fn f() {} } mod inner { pub fn g() {} }");
        assert!(inline_module_items(items, &["inner".to_string()]).is_some());
    }
}
