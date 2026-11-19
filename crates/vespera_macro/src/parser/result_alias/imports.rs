//! Whether a qualifier or a `use` brings a module, or a struct / enum named
//! like the alias, into the handler's scope: the import and local-glob
//! lookups [`super::is_external_path`] and [`super::find_alias`] need. A child
//! of `result_alias` (its helpers are reached through `super`).

use std::path::Path;

use crate::{collector::test_scope::attrs_test_only, schema_macro::file_cache};

use super::{Qualifier, Scope, declares_nominal, file_module, glob_parents, parsed_with};

/// Whether a local glob of `scope` (`use crate::models::*;`, `use super::*;`)
/// brings a module named `name` into scope: one of the modules it imports
/// from declares `mod name` ([`imported_declares`]). A glob that does not is
/// no reason to treat an un-imported qualifier as crate-local.
pub(super) fn glob_declares_module(name: &str, scope: &Scope<'_>, src_dir: Option<&Path>) -> bool {
    glob_parents(scope.items())
        .iter()
        .any(|parent| imported_declares(name, parent, scope, src_dir, declares_mod))
}

/// Whether `items` declare `mod name` (inline or file module).
pub(super) fn declares_mod(items: &[syn::Item], name: &str) -> bool {
    items
        .iter()
        .any(|item| matches!(item, syn::Item::Mod(module) if module.ident == name))
}

/// The item test a [`imported_declares`] / [`module_declares`] lookup runs
/// over a module's items: a struct / enum / union `name`
/// ([`declares_nominal`]) or a `mod name` ([`declares_mod`]).
pub(super) type Declares = fn(&[syn::Item], &str) -> bool;

/// Whether the module a `use` imports `name` from ([`import_parent`])
/// declares a struct / enum / union `name`: a named module (see
/// [`module_declares`]), the crate root (the top level of `lib.rs` /
/// `main.rs` under `src_dir`), or the parent module (the inline parent in
/// the handler's file; for a file module, any file's top level — the parent
/// file is not known).
pub(super) fn imported_nominal(
    name: &str,
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
) -> bool {
    imported_declares(name, parent, scope, src_dir, declares_nominal)
}

/// [`imported_nominal`] for an arbitrary item test: whether the module `parent`
/// names declares `name` as `declares` sees it.
pub(super) fn imported_declares(
    name: &str,
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    declares: Declares,
) -> bool {
    let top_level = |root_only: bool| {
        src_dir.is_some_and(|src_dir| {
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                (!root_only || file_module(path, src_dir).is_none())
                    && parsed_with(path, name).is_some_and(|file| declares(&file.items, name))
            })
        })
    };
    match parent {
        Qualifier::Module(module) => module_declares(name, module, scope, src_dir, declares),
        Qualifier::CrateRoot => top_level(true),
        Qualifier::Unresolved => false,
        Qualifier::Super => match scope.module.split_last() {
            Some((_, parent)) => {
                let parent = Scope {
                    file: scope.file.clone(),
                    module: parent.to_vec(),
                    path: scope.path.clone(),
                };
                declares(parent.items(), name)
            }
            None => top_level(false),
        },
    }
}

/// Whether a module named `module` (an inline `mod` of the handler's file,
/// or a file / inline module under `src_dir`) declares `name` as `declares`
/// sees it.
pub(super) fn module_declares(
    name: &str,
    module: &str,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
    declares: Declares,
) -> bool {
    fn walk(
        items: &[syn::Item],
        own: Option<&str>,
        name: &str,
        module: &str,
        declares: Declares,
    ) -> bool {
        (own == Some(module) && declares(items, name))
            || items.iter().any(|item| match item {
                syn::Item::Mod(syn::ItemMod {
                    attrs,
                    ident,
                    content: Some((_, inner)),
                    ..
                }) if !attrs_test_only(attrs) => {
                    walk(inner, Some(&ident.to_string()), name, module, declares)
                }
                _ => false,
            })
    }
    walk(&scope.file.items, None, name, module, declares)
        || src_dir.is_some_and(|src_dir| {
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                parsed_with(path, name).is_some_and(|file| {
                    walk(
                        &file.items,
                        file_module(path, src_dir).as_deref(),
                        name,
                        module,
                        declares,
                    )
                })
            })
        })
}
