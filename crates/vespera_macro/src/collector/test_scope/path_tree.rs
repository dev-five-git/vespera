//! The files a `#[path]` module loads, found by walking the module tree from
//! its target file.
//!
//! rustc's directory rules, checked against rustc (`cargo check` of a
//! `#[path = "beside/support.rs"] mod s;` crate whose `support.rs` declares
//! `mod child;`, which resolves to `beside/child.rs`, not
//! `beside/support/child.rs`):
//!
//! - A file loaded through a `#[path]` attribute owns the directory it sits
//!   in for its own `mod`s, whatever its name (`beside/support.rs` ->
//!   `beside/`).
//! - A by-name `mod x;` loads `<dir>/x.rs` or `<dir>/x/mod.rs`, where `<dir>`
//!   is the module's directory: a mod-rs file's directory, or the directory
//!   named after a non-mod-rs file (`routes.rs` -> `routes/`).
//! - A `#[path]` on a module outside inline blocks is relative to the
//!   directory of the file declaring it; inside an inline `mod a { .. }` it is
//!   relative to the module's directory (`<dir>/a/`).
//!
//! The tree is walked from the target, so it covers each child module of a
//! path-loaded file, however deep. Ungated edges only, for the modules every
//! build loads ([`Reach::OpenOnly`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use syn::Item;

use super::{attrs_test_only, lexical, path_attr, same_spelling_key};
use crate::schema_macro::file_cache;

/// Which edges of a `#[path]` target's module tree a walk follows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Reach {
    /// Only the `mod` declarations no `cfg(test)` build can leave out.
    OpenOnly,
    /// Every `mod` declaration the tree holds, gated or not.
    Any,
}

/// One `mod` declaration of a file.
#[derive(Clone)]
struct ModDecl {
    /// The inline `mod name { .. }` blocks it sits in, outermost first.
    inline: Vec<String>,
    /// Its own name (unraw).
    name: String,
    /// The value of its `#[path]`, when it is a string literal.
    path: Option<String>,
    /// Gated by a test-only `cfg`, on it or on an enclosing inline module.
    gated: bool,
}

/// A file's `mod` declarations (kept while the file is unchanged).
#[derive(Default)]
struct ModDecls(Vec<ModDecl>);

/// The `mod` declarations of `file` (none when it is unreadable or does not
/// parse, or holds no `mod` text at all).
fn mod_decls(file: &Path) -> std::rc::Rc<ModDecls> {
    file_cache::derived(file, |content| {
        let mut decls = Vec::new();
        if content.contains("mod")
            && let Ok(ast) = syn::parse_file(content)
        {
            collect(&ast.items, &mut Vec::new(), false, &mut decls);
        }
        ModDecls(decls)
    })
    .unwrap_or_default()
}

/// Collect the `mod` declarations among `items`, recursing into inline
/// modules (`inline` names the blocks around `items`; `within_gated` says
/// they sit in a test-only inline module).
fn collect(items: &[Item], inline: &mut Vec<String>, within_gated: bool, out: &mut Vec<ModDecl>) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        let name = syn::ext::IdentExt::unraw(&module.ident).to_string();
        let gated = within_gated || attrs_test_only(&module.attrs);
        match &module.content {
            Some((_, nested)) => {
                inline.push(name);
                collect(nested, inline, gated, out);
                inline.pop();
            }
            None => out.push(ModDecl {
                inline: inline.clone(),
                name,
                path: path_attr(&module.attrs),
                gated,
            }),
        }
    }
}

/// A file of a module tree and the directory its child modules are loaded
/// from.
struct Node {
    file: PathBuf,
    children_dir: PathBuf,
}

/// The [`same_spelling_key`]s of `target` and of every file its module tree
/// loads (through the edges `reach` allows). The target is taken as loaded by
/// a `#[path]` attribute, so its children sit beside it. Files a `mod` names
/// but which do not exist simply hold no declarations.
pub(super) fn tree_of(target: &Path, reach: Reach) -> HashSet<PathBuf> {
    let start = lexical(target).unwrap_or_else(|| target.to_path_buf());
    let start_dir = start.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut found = HashSet::new();
    let mut pending = vec![Node {
        file: start,
        children_dir: start_dir,
    }];
    while let Some(Node { file, children_dir }) = pending.pop() {
        let key = same_spelling_key(&file).unwrap_or_else(|| file.clone());
        if found.insert(key) {
            pending.extend(children(&file, &children_dir, reach));
        }
    }
    found
}

/// The files the `mod` declarations of `file` load (`children_dir` is the
/// directory its child modules are loaded from), through the edges `reach`
/// allows: a `#[path]` names its file (outside inline blocks relative to the
/// declaring file's directory, inside them to the inline module's), a by-name
/// `mod` loads `<dir>/<name>.rs` or `<dir>/<name>/mod.rs`.
fn children(file: &Path, children_dir: &Path, reach: Reach) -> Vec<Node> {
    let mut nodes = Vec::new();
    let parent = file.parent().map(Path::to_path_buf).unwrap_or_default();
    for decl in mod_decls(file)
        .0
        .iter()
        .filter(|decl| reach == Reach::Any || !decl.gated)
    {
        let inline_dir = decl
            .inline
            .iter()
            .fold(children_dir.to_path_buf(), |dir, block| dir.join(block));
        if let Some(path_value) = &decl.path {
            let base = if decl.inline.is_empty() {
                &parent
            } else {
                &inline_dir
            };
            if let Some(child) = lexical(&base.join(path_value)) {
                nodes.push(Node {
                    children_dir: child.parent().map(Path::to_path_buf).unwrap_or_default(),
                    file: child,
                });
            }
        } else {
            let child_dir = inline_dir.join(&decl.name);
            nodes.push(Node {
                file: inline_dir.join(format!("{}.rs", decl.name)),
                children_dir: child_dir.clone(),
            });
            nodes.push(Node {
                file: child_dir.join("mod.rs"),
                children_dir: child_dir,
            });
        }
    }
    nodes
}
