//! Whether a qualifier or a `use` brings a module, or a struct / enum named
//! like the alias, into the handler's scope: the import and local-glob
//! lookups [`super::is_external_path`] and [`super::find_alias`] need. A child
//! of `result_alias` (its helpers are reached through `super`).

use std::{cell::RefCell, path::Path};

use crate::{
    collector::test_scope::{TestScope, attrs_test_only},
    schema_macro::file_cache,
};

use super::{
    Import, MAX_ALIAS_DEPTH, Qualifier, Scope, alias_item, declares_nominal, file_module,
    glob_parent_entries, glob_parents, import_kind, import_parent, parse_items, renamed_import,
    renames_to,
};

/// Whether `items` declare `mod name` (inline or file module). A `#[cfg(test)]`
/// declaration does not count: the module is absent from a non-test build.
pub(super) fn declares_mod(items: &[syn::Item], name: &str) -> bool {
    items.iter().any(|item| match item {
        syn::Item::Mod(module) => module.ident == name && !attrs_test_only(&module.attrs),
        _ => false,
    })
}

/// Whether a local glob of `scope` (`use crate::models::*;`, `use super::*;`)
/// brings a module named `name` into scope: one of the modules it imports
/// from declares `mod name`. A glob that does not is no reason to treat an
/// un-imported qualifier as crate-local.
pub(super) fn glob_declares_module(name: &str, scope: &Scope<'_>, src_dir: Option<&Path>) -> bool {
    glob_parents(scope.items())
        .iter()
        .any(|parent| any_module_items(parent, scope, src_dir, &|items| declares_mod(items, name)))
}

/// Whether the module a `use` imports `name` from ([`super::import_parent`])
/// declares a struct / enum / union `name`, or re-exports one: a named module
/// (see [`module_items`]), the crate root (the top level of `lib.rs` /
/// `main.rs` under `src_dir`), or the parent module (the inline parent in
/// the handler's file; for a file module, any file's top level — the parent
/// file is not known). A re-export (`pub use models::ApiResponse;`, or a glob
/// of such a module) counts as the nominal type it names, see
/// [`reexports_nominal`].
pub(super) fn imported_nominal(
    name: &str,
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
) -> bool {
    nominal_in(name, parent, scope, src_dir, 0)
}

/// [`imported_nominal`] at re-export depth `depth`. A chain of re-exports
/// deeper than [`MAX_ALIAS_DEPTH`] (a cycle, in practice) is unresolvable and
/// counts as nominal: never a same-named alias elsewhere.
fn nominal_in(
    name: &str,
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    depth: usize,
) -> bool {
    if depth > MAX_ALIAS_DEPTH {
        return true;
    }
    any_module_items(parent, scope, src_dir, &|items| {
        declares_nominal(items, name)
    }) || (follows_reexports(parent, scope)
        && any_module_items(parent, scope, src_dir, &|items| {
            reexports_nominal(name, items, scope, src_dir, depth)
        }))
}

/// Whether the re-exports of the module `parent` names can be followed. The
/// top level of any file (`super::` from a file module) is not known to be
/// the handler's parent, so only the declarations there count.
fn follows_reexports(parent: &Qualifier, scope: &Scope<'_>) -> bool {
    !(matches!(parent, Qualifier::Super) && scope.module.is_empty())
}

/// Whether the module with the item list `items` brings `name` in as a
/// struct / enum / union through a `use` or a glob. An alias the module
/// declares itself is not nominal (the lookup keeps its unique-definition
/// fallback). A re-export that cannot be followed here (another crate, a
/// rename, a `super` import whose module is not known) counts as nominal:
/// the name is then not a same-named alias elsewhere in the crate.
fn reexports_nominal(
    name: &str,
    items: &[syn::Item],
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    depth: usize,
) -> bool {
    if items.iter().any(|item| alias_item(item, name).is_some()) {
        return false;
    }
    // A rename of a crate-root / named-module item is nominal unless it names
    // an alias that module declares (see `reexported_alias`): only then is the
    // alias expandable under the new name. Anything else stays nominal.
    if let Some((original, parent)) = renamed_source(items, name) {
        return !declares_alias(&parent, &original, scope, src_dir);
    }
    if renames_to(items, name) {
        return true;
    }
    match import_kind(items, name) {
        Some(Import::External) => true,
        // Only a crate-root / named-module source can be followed. A private
        // `use super::X;` brings the parent's `X` into this module only, like
        // a private `use super::*;` (see the glob arm): it is no re-export, so
        // it decides nothing. A `pub` one stays nominal (its source is unknown
        // here). An unresolved chain (`self`, `super::super`) stays nominal.
        Some(Import::Local) => import_parent(items, name).is_none_or(|parent| match parent {
            Qualifier::CrateRoot | Qualifier::Module(_) => {
                nominal_in(name, &parent, scope, src_dir, depth + 1)
            }
            Qualifier::Super => named_use_is_public(items, name),
            Qualifier::Unresolved => true,
        }),
        // A glob of an unresolved chain may bring a struct in from a module
        // this lookup cannot follow, so it counts as nominal. A `pub` glob of
        // `super` re-exports the parent's names (possibly a struct, which is
        // then nominal); a private one only brings them into this module.
        None => glob_parent_entries(items)
            .iter()
            .any(|(parent, public)| match parent {
                Qualifier::CrateRoot | Qualifier::Module(_) => {
                    nominal_in(name, parent, scope, src_dir, depth + 1)
                }
                Qualifier::Super => *public,
                Qualifier::Unresolved => true,
            }),
    }
}

/// Whether `check` holds for the item list of a module `parent` names: the
/// crate root's top level, a named module (see [`module_items`]), the
/// handler's parent (its inline parent, else any file's top level).
pub(super) fn any_module_items(
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    check: &dyn Fn(&[syn::Item]) -> bool,
) -> bool {
    // Test-only files (`#[cfg(test)] mod fixtures;`) never define an alias
    // or a struct the handler can name, so they are not searched.
    let top_level = |root_only: bool| {
        src_dir.is_some_and(|src_dir| {
            let mut test_scope = TestScope::new(Some(src_dir));
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                (!root_only || file_module(path, src_dir).is_none())
                    && !test_scope.file_test_only(path)
                    && parse_items(path).is_some_and(|file| check(&file.items))
            })
        })
    };
    match parent {
        Qualifier::Module(module) => module_items(module, scope, src_dir, check),
        Qualifier::CrateRoot => top_level(true),
        Qualifier::Unresolved => false,
        Qualifier::Super => match scope.module.split_last() {
            Some((_, parent)) => {
                let parent = Scope {
                    file: scope.file.clone(),
                    module: parent.to_vec(),
                    path: scope.path.clone(),
                };
                check(parent.items())
            }
            None => top_level(false),
        },
    }
}

/// [`any_module_items`] for a named module: an inline `mod` of the handler's
/// file, or a file / inline module under `src_dir`.
fn module_items(
    module: &str,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    check: &dyn Fn(&[syn::Item]) -> bool,
) -> bool {
    fn walk(
        items: &[syn::Item],
        own: Option<&str>,
        module: &str,
        check: &dyn Fn(&[syn::Item]) -> bool,
    ) -> bool {
        (own == Some(module) && check(items))
            || items.iter().any(|item| match item {
                syn::Item::Mod(syn::ItemMod {
                    attrs,
                    ident,
                    content: Some((_, inner)),
                    ..
                }) if !attrs_test_only(attrs) => {
                    walk(inner, Some(&ident.to_string()), module, check)
                }
                _ => false,
            })
    }
    walk(&scope.file.items, None, module, check)
        || src_dir.is_some_and(|src_dir| {
            let mut test_scope = TestScope::new(Some(src_dir));
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                !test_scope.file_test_only(path)
                    && parse_items(path).is_some_and(|file| {
                        walk(
                            &file.items,
                            file_module(path, src_dir).as_deref(),
                            module,
                            check,
                        )
                    })
            })
        })
}

/// The local rename `name` is among `items`: the item it renames and the
/// crate-root / named module it comes from (`pub use crate::error::AppResult
/// as ApiResponse;` -> `AppResult`, `error`). `None` for another crate, a
/// `super` / `self` source, or no such rename.
pub(super) fn renamed_source(items: &[syn::Item], name: &str) -> Option<(String, Qualifier)> {
    renamed_import(items, name)
        .filter(|_| import_kind(items, name) == Some(Import::Local))
        .and_then(|(original, parent)| match parent? {
            parent @ (Qualifier::CrateRoot | Qualifier::Module(_)) => Some((original, parent)),
            _ => None,
        })
}

/// The alias a re-export under another name stands for: the module `qualifier`
/// names renames `name` to an item (see [`renamed_source`]) that the module it
/// comes from declares as an alias (`pub use error::AppResult as ApiResponse;`
/// with `error` declaring `AppResult`). One hop only: a rename of a rename is
/// not followed here, so it keeps its nominal fallback.
pub(super) fn reexported_alias(
    qualifier: &Qualifier,
    name: &str,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
) -> Option<(String, Qualifier)> {
    let rename = RefCell::new(None);
    any_module_items(qualifier, scope, src_dir, &|items| {
        *rename.borrow_mut() = renamed_source(items, name);
        rename.borrow().is_some()
    });
    let (original, parent) = rename.into_inner()?;
    declares_alias(&parent, &original, scope, src_dir).then_some((original, parent))
}

/// Whether the module `parent` names declares a `type` alias called `name`.
fn declares_alias(
    parent: &Qualifier,
    name: &str,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
) -> bool {
    any_module_items(parent, scope, src_dir, &|items| {
        items.iter().any(|item| alias_item(item, name).is_some())
    })
}

/// Whether a `pub` / `pub(..)` `use` among `items` brings `name` into scope.
fn named_use_is_public(items: &[syn::Item], name: &str) -> bool {
    items.iter().any(|item| {
        matches!(item, syn::Item::Use(item_use)
            if !matches!(item_use.vis, syn::Visibility::Inherited)
                && use_binds(&item_use.tree, name))
    })
}

/// Whether `tree` brings `name` into scope under that name (`a::name`,
/// `a::b as name`, in a group); a glob names no single item.
fn use_binds(tree: &syn::UseTree, name: &str) -> bool {
    match tree {
        syn::UseTree::Path(path) => use_binds(&path.tree, name),
        syn::UseTree::Name(used) => used.ident == name,
        syn::UseTree::Rename(rename) => rename.rename == name,
        syn::UseTree::Group(group) => group.items.iter().any(|tree| use_binds(tree, name)),
        syn::UseTree::Glob(_) => false,
    }
}
