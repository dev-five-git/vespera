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
//! - An inline `#[path = "d"] mod h { .. }` names the directory its children
//!   load from (`<dir>/d/`, not `<dir>/h/`): the `#[path]` value replaces the
//!   module name as that block's directory segment.
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

/// One inline `mod name { .. }` block a `mod` declaration sits in.
#[derive(Clone)]
struct Block {
    /// Unique within its file (the blocks are numbered in source order).
    id: usize,
    /// The directory segment its children load from: its `#[path]` value, or
    /// its name.
    dir: String,
}

/// One `mod` declaration of a file.
#[derive(Clone)]
struct ModDecl {
    /// The inline `mod name { .. }` blocks it sits in, outermost first.
    inline: Vec<Block>,
    /// Its own name (unraw).
    name: String,
    /// The value of its `#[path]`, when it is a string literal.
    path: Option<String>,
    /// Gated by a test-only `cfg`, on it or on an enclosing inline module.
    gated: bool,
}

/// An inline module that carries a `#[path]` (`#[path = "d"] mod h { .. }`).
/// It names no file, but the files its own `mod`s load are a tree of their
/// own, walked from it as a [`TreeRoot::Block`].
#[derive(Clone)]
struct PathBlock {
    id: usize,
    gated: bool,
}

/// The `mod` declarations of a file.
#[derive(Default)]
struct ModDecls {
    decls: Vec<ModDecl>,
    path_blocks: Vec<PathBlock>,
}

/// The `mod` declarations of `file` (none when it is unreadable or does not
/// parse, or holds no `mod` text at all).
fn mod_decls(file: &Path) -> std::rc::Rc<ModDecls> {
    file_cache::derived(file, |content| {
        let mut collector = Collector::default();
        if content.contains("mod")
            && let Ok(ast) = syn::parse_file(content)
        {
            collector.items(&ast.items, &mut Vec::new(), false);
        }
        ModDecls {
            decls: collector.decls,
            path_blocks: collector.path_blocks,
        }
    })
    .unwrap_or_default()
}

/// Gathers a file's [`ModDecls`] while walking its items.
#[derive(Default)]
struct Collector {
    decls: Vec<ModDecl>,
    path_blocks: Vec<PathBlock>,
    /// The inline blocks numbered so far.
    blocks: usize,
}

impl Collector {
    /// Collect the `mod` declarations among `items`, recursing into inline
    /// modules (`inline` names the blocks around `items`; `within_gated` says
    /// they sit in a test-only inline module).
    fn items(&mut self, items: &[Item], inline: &mut Vec<Block>, within_gated: bool) {
        for item in items {
            let Item::Mod(module) = item else { continue };
            let name = syn::ext::IdentExt::unraw(&module.ident).to_string();
            let gated = within_gated || attrs_test_only(&module.attrs);
            let path = path_attr(&module.attrs);
            match &module.content {
                Some((_, nested)) => {
                    let id = self.blocks;
                    self.blocks += 1;
                    if path.is_some() {
                        self.path_blocks.push(PathBlock { id, gated });
                    }
                    inline.push(Block {
                        id,
                        dir: path.unwrap_or(name),
                    });
                    self.items(nested, inline, gated);
                    inline.pop();
                }
                None => self.decls.push(ModDecl {
                    inline: inline.clone(),
                    name,
                    path,
                    gated,
                }),
            }
        }
    }
}

/// Where a module tree walk starts.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum TreeRoot {
    /// A file a `#[path]` attribute loads; its children sit beside it.
    File(PathBuf),
    /// The inline module `id` of `declarer` that carries a `#[path]`: the
    /// files its own `mod`s load (`children_dir` is the directory `declarer`
    /// loads its children from).
    Block {
        declarer: PathBuf,
        children_dir: PathBuf,
        id: usize,
    },
}

/// A file of a module tree and the directory its child modules are loaded
/// from.
struct Node {
    file: PathBuf,
    children_dir: PathBuf,
}

/// The [`same_spelling_key`]s of the files a walk from `root` loads (through
/// the edges `reach` allows). A [`TreeRoot::File`] is taken as loaded by a
/// `#[path]` attribute, so its children sit beside it. Files a `mod` names
/// but which do not exist simply hold no declarations.
pub(super) fn tree_of(root: &TreeRoot, reach: Reach) -> HashSet<PathBuf> {
    let mut pending = start_nodes(root, reach);
    let mut found = HashSet::new();
    while let Some(Node { file, children_dir }) = pending.pop() {
        let key = same_spelling_key(&file).unwrap_or_else(|| file.clone());
        if found.insert(key) {
            pending.extend(children(&file, &children_dir, reach, None));
        }
    }
    found
}

/// The nodes a walk from `root` starts with.
fn start_nodes(root: &TreeRoot, reach: Reach) -> Vec<Node> {
    match root {
        TreeRoot::File(target) => {
            let start = lexical(target).unwrap_or_else(|| target.clone());
            let start_dir = start.parent().map(Path::to_path_buf).unwrap_or_default();
            vec![Node {
                file: start,
                children_dir: start_dir,
            }]
        }
        TreeRoot::Block {
            declarer,
            children_dir,
            id,
        } => children(declarer, children_dir, reach, Some(*id)),
    }
}

/// The files the `mod` declarations of `file` load (`children_dir` is the
/// directory its child modules are loaded from), through the edges `reach`
/// allows. `within` keeps only the declarations inside the inline block with
/// that id.
fn children(file: &Path, children_dir: &Path, reach: Reach, within: Option<usize>) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mods = mod_decls(file);
    for decl in mods.decls.iter().filter(|decl| {
        (reach == Reach::Any || !decl.gated)
            && within.is_none_or(|id| decl.inline.iter().any(|block| block.id == id))
    }) {
        if decl.path.is_some() {
            if let Some(child) = path_target(file, children_dir, decl) {
                nodes.push(Node {
                    children_dir: child.parent().map(Path::to_path_buf).unwrap_or_default(),
                    file: child,
                });
            }
        } else {
            // A by-name child: `<dir>/<name>.rs` or `<dir>/<name>/mod.rs` (the
            // one that exists is the module; the other holds no declarations).
            let child_dir = inline_dir(children_dir, decl).join(&decl.name);
            nodes.push(Node {
                file: child_dir.with_extension("rs"),
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

/// The directory a `mod` declaration of a file with child directory
/// `children_dir` is resolved in: its inline blocks nest under it, each by
/// its directory segment (`#[path]` value or name).
fn inline_dir(children_dir: &Path, decl: &ModDecl) -> PathBuf {
    decl.inline
        .iter()
        .fold(children_dir.to_path_buf(), |dir, block| {
            dir.join(&block.dir)
        })
}

/// The file a `#[path]` declaration names, the one rustc resolves: outside
/// inline blocks relative to the declaring file's directory, inside them to
/// the inline module's directory. A declaration with no `#[path]` names none.
fn path_target(file: &Path, children_dir: &Path, decl: &ModDecl) -> Option<PathBuf> {
    let path_value = decl.path.as_deref()?;
    let base = if decl.inline.is_empty() {
        file.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        inline_dir(children_dir, decl)
    };
    lexical(&base.join(path_value))
}

/// A `#[path]` declaration of the crate: the file declaring it, what it names
/// ([`TreeRoot::File`] for a `mod` with a `#[path]`, [`TreeRoot::Block`] for
/// an inline module with one), and whether a test-only `cfg` gates it.
pub(super) struct PathDecl {
    pub(super) declarer: PathBuf,
    pub(super) target: TreeRoot,
    pub(super) gated: bool,
}

/// Every `#[path]` declaration in the module tree of the crate whose sources
/// are under `src_dir`. The tree is walked from `lib.rs` / `main.rs` with
/// rustc's directory rules and every `cfg` ignored (a gated declaration is
/// listed with `gated`). A file no declaration reaches is not compiled, so its
/// declarations are not listed.
pub(super) fn path_declarations(src_dir: &Path) -> Vec<PathDecl> {
    let mut decls = Vec::new();
    let mut found = HashSet::new();
    let mut pending: Vec<Node> = ["lib.rs", "main.rs"]
        .iter()
        .map(|root| Node {
            file: src_dir.join(root),
            children_dir: src_dir.to_path_buf(),
        })
        .collect();
    while let Some(Node { file, children_dir }) = pending.pop() {
        let key = same_spelling_key(&file).unwrap_or_else(|| file.clone());
        if found.insert(key) {
            let mods = mod_decls(&file);
            for decl in &mods.decls {
                if let Some(target) = path_target(&file, &children_dir, decl) {
                    decls.push(PathDecl {
                        declarer: file.clone(),
                        target: TreeRoot::File(target),
                        gated: decl.gated,
                    });
                }
            }
            for block in &mods.path_blocks {
                decls.push(PathDecl {
                    declarer: file.clone(),
                    target: TreeRoot::Block {
                        declarer: file.clone(),
                        children_dir: children_dir.clone(),
                        id: block.id,
                    },
                    gated: block.gated,
                });
            }
            pending.extend(children(&file, &children_dir, Reach::Any, None));
        }
    }
    decls
}
