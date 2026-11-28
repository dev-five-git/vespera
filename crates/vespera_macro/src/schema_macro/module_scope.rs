//! Module-tree resolution and per-module name scopes.
//!
//! `schema_type!` copies field types and serde function paths out of a source
//! struct that usually lives in ANOTHER module, so every name written there
//! must be re-expressed as a path that resolves at the macro call site. That
//! needs two things this module provides:
//!
//! - **Where a module lives** ([`module_items`]): `crate::a::b` is looked up
//!   in `src/a/b.rs` / `src/a/b/mod.rs` (fast path, also for crates without a
//!   root file on disk), then by walking the module tree from the crate root
//!   (`src/lib.rs` / `src/main.rs`, or the test / example / bench / binary
//!   root, see [`super::crate_layout`]) through `mod x;` declarations — honouring
//!   `#[path = "..."]` — and inline `mod x { ... }` blocks.
//! - **What a name means inside it** ([`ModuleScope`] / [`resolve_name`]):
//!   `use` imports (private imports must not be re-exported through the
//!   module path: `E0603`), locally defined items, child modules and glob
//!   imports.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenStream;
use quote::quote;

use super::file_cache;

mod declared;
pub use declared::declared_module_of;
mod visibility;
pub(in crate::schema_macro) use visibility::{path_readable_from, readable_from};

/// Items of one module plus the location state needed to descend further.
#[derive(Clone)]
struct Cursor {
    items: Vec<syn::Item>,
    /// Directory of the file the items come from.
    file_dir: PathBuf,
    /// `lib.rs` / `main.rs` / `mod.rs` / `#[path]`-loaded file.
    mod_rs: bool,
    /// File stem of a non-mod-rs file (`b` for `a/b.rs`).
    stem: String,
    /// Inline `mod x { .. }` components walked inside the current file.
    inline: Vec<String>,
    /// The file the items come from.
    file: PathBuf,
}

fn open_file(path: &Path, force_mod_rs: bool) -> Option<Cursor> {
    let file = file_cache::get_parsed_file(path)?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    // A `lib.rs` / `main.rs` below the crate root is an ordinary module
    // file (rustc reads its children from `<dir>/main/`); the roots are
    // opened with `force_mod_rs`.
    let mod_rs = force_mod_rs || stem == "mod";
    Some(Cursor {
        file: path.to_path_buf(),
        items: file.items,
        file_dir: path.parent()?.to_path_buf(),
        mod_rs,
        stem,
        inline: Vec::new(),
    })
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(nv) = &attr.meta else {
            return None;
        };
        if !nv.path.is_ident("path") {
            return None;
        }
        match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit),
                ..
            }) => Some(lit.value()),
            _ => None,
        }
    })
}

fn ident_name(ident: &syn::Ident) -> String {
    crate::parser::strip_raw_prefix_owned(ident.to_string())
}

/// Step from `cursor` into its child module `segment` (a cfg twin the
/// compiler drops is skipped: `#[cfg(feature = "pg")] #[path = ..] mod m;`).
fn descend(cursor: Cursor, segment: &str) -> Option<Cursor> {
    let module = cursor.items.iter().find_map(|item| match item {
        syn::Item::Mod(module)
            if ident_name(&module.ident) == segment
                && !super::cfg_eval::compiled_out(&module.attrs) =>
        {
            Some(module)
        }
        _ => None,
    })?;
    let custom_path = path_attr(&module.attrs);
    if let Some((_, items)) = &module.content {
        let mut inline = cursor.inline.clone();
        inline.push(custom_path.unwrap_or_else(|| segment.to_string()));
        return Some(Cursor {
            items: items.clone(),
            inline,
            ..cursor
        });
    }
    let mut base = if cursor.mod_rs {
        cursor.file_dir.clone()
    } else {
        cursor.file_dir.join(&cursor.stem)
    };
    base.extend(&cursor.inline);
    if let Some(custom) = custom_path {
        let file = if cursor.inline.is_empty() {
            cursor.file_dir.join(custom)
        } else {
            base.join(custom)
        };
        return open_file(&file, true);
    }
    open_file(&base.join(format!("{segment}.rs")), false)
        .or_else(|| open_file(&base.join(segment).join("mod.rs"), true))
}

/// Module segments below the crate root (`crate::a::b` -> `[a, b]`), or
/// `None` for relative (`self` / `super`) paths and for an external
/// `::krate::..` path (its leading empty segment, R22-SM3-02: `Path::join`
/// would probe a rooted `/entities/..` file instead of the crate's `src`).
fn crate_relative(module_path: &[String]) -> Option<&[String]> {
    let rest = match module_path.first().map(String::as_str) {
        Some("crate") => &module_path[1..],
        _ => module_path,
    };
    (!rest
        .iter()
        .any(|s| s.is_empty() || s == "self" || s == "super" || s == "crate"))
    .then_some(rest)
}

/// Whether `src/a/b.rs` (below `src`) can stand for the module
/// `crate::a::b` (`segments`): no parent module file on disk (the crate
/// roots `roots` for `a`, `src/a.rs` / `src/a/mod.rs` for `b`) declares a
/// segment with a `#[path]` redirect (or one `#[cfg]` twin of it) or as an
/// inline `mod b { .. }` — the module then lives elsewhere and only the
/// declaration-aware tree walk finds it. A parent that is not on disk
/// keeps the fast path (trees without a crate root).
pub fn fast_path_is_plain<S: AsRef<str>>(src: &Path, roots: &[PathBuf], segments: &[S]) -> bool {
    segments.iter().enumerate().all(|(level, segment)| {
        let segment = segment.as_ref();
        let parents: Vec<PathBuf> = if level == 0 {
            roots.to_vec()
        } else {
            let joined: Vec<&str> = segments[..level].iter().map(AsRef::as_ref).collect();
            let joined = joined.join("/");
            vec![
                src.join(format!("{joined}.rs")),
                src.join(joined).join("mod.rs"),
            ]
        };
        !parents.iter().any(|parent| redirected(parent, segment))
    })
}

/// Names of a file's top-level `mod`s declared inline or with `#[path]`
/// (empty for a file that does not parse), kept while it is unchanged.
struct Redirected(HashSet<String>);

/// Whether `file` declares `segment` inline or with `#[path]`.
fn redirected(file: &Path, segment: &str) -> bool {
    file_cache::derived(file, redirected_mods).is_some_and(|names| names.0.contains(segment))
}

fn redirected_mods(content: &str) -> Redirected {
    let names = file_cache::with_fallback_parse(content, |file| {
        file.items.iter().filter_map(redirected_name).collect()
    });
    Redirected(names.unwrap_or_default())
}

fn redirected_name(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::Mod(module)
            if module.content.is_some() || path_attr(&module.attrs).is_some() =>
        {
            Some(ident_name(&module.ident))
        }
        _ => None,
    }
}

/// The items of the module at `module_path` (`crate::a::b` or `a::b`).
pub fn module_items(module_path: &[String]) -> Option<Vec<syn::Item>> {
    let segments = crate_relative(module_path)?;
    let layout = super::crate_layout::crate_layout()?;
    let src = &layout.module_dir;
    // Fast path: the longest prefix that is a module file of its own, then
    // inline / nested modules below it. Works for trees without a crate root
    // and costs no root walk for the common `src/a/b.rs` layout. A prefix a
    // parent declares with `#[path]` / inline is no file of its own.
    for split in (1..=segments.len()).rev() {
        if !fast_path_is_plain(src, &layout.roots, &segments[..split]) {
            continue;
        }
        let joined = segments[..split].join("/");
        let file = open_file(&src.join(format!("{joined}.rs")), false)
            .or_else(|| open_file(&src.join(&joined).join("mod.rs"), true));
        if let Some(cursor) = file {
            if let Some(found) = descend_all(cursor, &segments[split..]) {
                return Some(found.items);
            }
            break;
        }
    }
    layout
        .roots
        .iter()
        .find_map(|root| descend_all(open_file(root, true)?, segments))
        .map(|cursor| cursor.items)
}

/// The absolute module (`crate::..`) of `file` in the crate being compiled
/// (`None` without a crate layout).
pub fn file_module(file: &Path) -> Option<Vec<String>> {
    Some(super::crate_layout::crate_layout()?.module_path_of(file))
}

fn descend_all(mut cursor: Cursor, segments: &[String]) -> Option<Cursor> {
    for segment in segments {
        cursor = descend(cursor, segment)?;
    }
    Some(cursor)
}

/// Token-string definition of the top-level struct `name` in `items` (one
/// the compiler drops by `#[cfg]` is skipped).
pub fn struct_in_items(items: &[syn::Item], name: &str) -> Option<String> {
    items.iter().find_map(|item| match item {
        syn::Item::Struct(item)
            if ident_name(&item.ident) == name && !super::cfg_eval::compiled_out(&item.attrs) =>
        {
            Some(quote!(#item).to_string())
        }
        _ => None,
    })
}

/// The inline `mod` chain of the struct / enum `name` in `items`: the only
/// one of that name, or the one whose ident starts at `line`; empty (the
/// file's top level) when it is not found — a `schema_type!` DTO is not
/// written in the file at all.
#[cfg(test)]
pub fn item_module_chain(items: &[syn::Item], name: &str, line: usize) -> Vec<String> {
    pick_chain(chain_candidates(items, name), line)
}

/// [`item_module_chain`] for `file`, read off its cached line index: a
/// repeated name is told apart by line, which only a parse with
/// proc-macro2's fallback forced reports (inside the compiler every token of
/// re-parsed text carries the call site's span).  Items inside fn bodies
/// count (SM0-L3), with the inline modules around the fn. `None` when `file`
/// cannot be read or parsed.
pub fn item_module_chain_cached(file: &Path, name: &str, line: usize) -> Option<Vec<String>> {
    let lines = file_lines(file)?;
    let found = lines
        .items
        .iter()
        .filter(|(item, ..)| item == name)
        .map(|(_, at, chain)| (*at, chain.clone()))
        .collect();
    Some(pick_chain(found, line))
}

/// The lines the struct / enum `name` is declared on in the inline module
/// `chain` of `file`, read off the same cached index as
/// [`item_module_chain_cached`] (fn bodies included). `None` when `file`
/// cannot be read or parsed.
pub fn item_lines_in_chain(file: &Path, name: &str, chain: &[String]) -> Option<Vec<usize>> {
    let lines = file_lines(file)?;
    Some(
        lines
            .items
            .iter()
            .filter(|(item, _, item_chain)| item == name && item_chain == chain)
            .map(|(_, at, _)| *at)
            .collect(),
    )
}

/// Line-accurate plain data of one file, read off a parse with
/// proc-macro2's fallback forced (inside the compiler every token of
/// re-parsed text carries the call site's span) and kept while the file is
/// unchanged (`file_cache::derived`).
#[derive(Default)]
struct FileLines {
    /// Every struct / enum (inline modules included, in source order): its
    /// name, ident line and inline `mod` chain (see [`chain_candidates`]).
    items: Vec<(String, usize, Vec<String>)>,
    /// The inline `mod { .. }` blocks: name, brace line range, children.
    mods: Vec<InlineMod>,
}

struct InlineMod {
    name: String,
    lines: std::ops::RangeInclusive<usize>,
    inner: Vec<Self>,
}

/// The cached [`FileLines`] of a file: `None` when it does not parse.
struct CachedLines(Option<std::rc::Rc<FileLines>>);

/// `file`'s [`FileLines`]; `None` when it cannot be read or parsed.
fn file_lines(file: &Path) -> Option<std::rc::Rc<FileLines>> {
    fn mods(items: &[syn::Item]) -> Vec<InlineMod> {
        items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Mod(syn::ItemMod {
                    ident,
                    content: Some((brace, inner)),
                    ..
                }) => Some(InlineMod {
                    name: ident_name(ident),
                    lines: brace.span.open().start().line..=brace.span.close().end().line,
                    inner: mods(inner),
                }),
                _ => None,
            })
            .collect()
    }
    let cached = file_cache::derived(file, |content| {
        CachedLines(
            file_cache::with_fallback_parse(content, |parsed| FileLines {
                items: all_chain_candidates(&parsed.items),
                mods: mods(&parsed.items),
            })
            .map(std::rc::Rc::new),
        )
    })?;
    cached.0.clone()
}

/// The inline `mod` chain whose braces enclose `line` of `file` — where a
/// macro invoked on that line resolves its names (empty at the file's top
/// level, for line `0` or an unreadable file).
pub fn inline_chain_at(file: &Path, line: usize) -> Vec<String> {
    fn enclosing(mods: &[InlineMod], line: usize, chain: &mut Vec<String>) {
        if let Some(module) = mods.iter().find(|module| module.lines.contains(&line)) {
            chain.push(module.name.clone());
            enclosing(&module.inner, line, chain);
        }
    }
    if line == 0
        || !file_cache::get_file_content(file).is_some_and(|content| content.contains("mod"))
    {
        return Vec::new();
    }
    let mut chain = Vec::new();
    if let Some(lines) = file_lines(file) {
        enclosing(&lines.mods, line, &mut chain);
    }
    chain
}

/// The only candidate's chain, or (several) the one whose ident is at `line`;
/// empty (the file's top level) when no candidate is left.
fn pick_chain(mut found: Vec<(usize, Vec<String>)>, line: usize) -> Vec<String> {
    if found.len() > 1 {
        found.retain(|(found_line, _)| *found_line == line);
    }
    match found.as_mut_slice() {
        [(_, chain)] => std::mem::take(chain),
        _ => Vec::new(),
    }
}

/// Every struct / enum `name` in `items` (inline modules included): its
/// ident's line and inline `mod` chain.
#[cfg(test)]
fn chain_candidates(items: &[syn::Item], name: &str) -> Vec<(usize, Vec<String>)> {
    let mut found = Vec::new();
    walk_chains(items, &mut Vec::new(), &mut |ident, chain| {
        if ident_name(ident) == name {
            found.push((ident.span().start().line, chain.to_vec()));
        }
    });
    found
}

/// [`chain_candidates`] of every name at once: `(name, line, chain)`.
fn all_chain_candidates(items: &[syn::Item]) -> Vec<(String, usize, Vec<String>)> {
    let mut found = Vec::new();
    walk_chains(items, &mut Vec::new(), &mut |ident, chain| {
        found.push((ident_name(ident), ident.span().start().line, chain.to_vec()));
    });
    found
}

/// Visit every struct / enum ident of `items` (inline modules, fn bodies and
/// the bodies of impl / trait methods included, in source order) with its
/// inline `mod` chain. A body's items take the chain of the module the
/// fn / impl / trait sits in (SM0-L3, R44-SM-L1).
fn walk_chains(
    items: &[syn::Item],
    chain: &mut Vec<String>,
    visit: &mut dyn FnMut(&syn::Ident, &[String]),
) {
    for item in items {
        let ident = match item {
            syn::Item::Struct(item) => &item.ident,
            syn::Item::Enum(item) => &item.ident,
            syn::Item::Mod(module) => {
                if let Some((_, inner)) = &module.content {
                    chain.push(ident_name(&module.ident));
                    walk_chains(inner, chain, visit);
                    chain.pop();
                }
                continue;
            }
            syn::Item::Fn(function) => {
                walk_body(&function.block, chain, visit);
                continue;
            }
            syn::Item::Impl(imp) => {
                for method in &imp.items {
                    if let syn::ImplItem::Fn(method) = method {
                        walk_body(&method.block, chain, visit);
                    }
                }
                continue;
            }
            syn::Item::Trait(tr) => {
                for method in &tr.items {
                    if let syn::TraitItem::Fn(syn::TraitItemFn {
                        default: Some(block),
                        ..
                    }) = method
                    {
                        walk_body(block, chain, visit);
                    }
                }
                continue;
            }
            _ => continue,
        };
        visit(ident, chain);
    }
}

/// [`walk_chains`] over the items a block declares.
fn walk_body(
    block: &syn::Block,
    chain: &mut Vec<String>,
    visit: &mut dyn FnMut(&syn::Ident, &[String]),
) {
    let mut body = BodyItems(Vec::new());
    syn::visit::Visit::visit_block(&mut body, block);
    walk_chains(&body.0, chain, visit);
}

/// The items a block declares, at any depth of its statements. Each item is
/// taken whole (not descended into): [`walk_chains`] visits its own nested
/// items.
struct BodyItems(Vec<syn::Item>);

impl<'ast> syn::visit::Visit<'ast> for BodyItems {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        self.0.push(item.clone());
    }
}

/// The struct `name` defined anywhere in `items` (inline modules included)
/// when exactly one such struct exists, with the inline module chain.
/// Structs and inline modules the compiler drops by `#[cfg]` do not count.
pub fn unique_nested_struct(items: &[syn::Item], name: &str) -> Option<(String, Vec<String>)> {
    fn walk(
        items: &[syn::Item],
        name: &str,
        chain: &mut Vec<String>,
        found: &mut Vec<(String, Vec<String>)>,
    ) {
        for item in items {
            match item {
                syn::Item::Struct(item)
                    if ident_name(&item.ident) == name
                        && !super::cfg_eval::compiled_out(&item.attrs) =>
                {
                    found.push((quote!(#item).to_string(), chain.clone()));
                }
                syn::Item::Mod(module) if !super::cfg_eval::compiled_out(&module.attrs) => {
                    if let Some((_, inner)) = &module.content {
                        chain.push(ident_name(&module.ident));
                        walk(inner, name, chain, found);
                        chain.pop();
                    }
                }
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    walk(items, name, &mut Vec::new(), &mut found);
    if found.len() == 1 { found.pop() } else { None }
}

/// The names visible inside one module.
#[derive(Debug, Default)]
pub struct ModuleScope {
    /// `use` imports: local name -> normalized path.
    pub imports: HashMap<String, Vec<String>>,
    /// Items defined in the module (types, fns, consts, statics, traits, mods).
    pub locals: HashSet<String>,
    /// Normalized bases of `use base::*` imports.
    pub globs: Vec<Vec<String>>,
    /// Imports / items declared without `pub`: a glob import only brings
    /// them into the module's own descendants.
    pub private: HashSet<String>,
    /// Each item's readable scope (`visibility.rs`): `None` for a `pub` item.
    read_scopes: HashMap<String, Option<Vec<String>>>,
}

/// The ident an item defines in its module's type / value namespace.
fn item_ident(item: &syn::Item) -> Option<(&syn::Ident, &syn::Visibility)> {
    Some(match item {
        syn::Item::Struct(i) => (&i.ident, &i.vis),
        syn::Item::Enum(i) => (&i.ident, &i.vis),
        syn::Item::Union(i) => (&i.ident, &i.vis),
        syn::Item::Type(i) => (&i.ident, &i.vis),
        syn::Item::Trait(i) => (&i.ident, &i.vis),
        syn::Item::Fn(i) => (&i.sig.ident, &i.vis),
        syn::Item::Const(i) => (&i.ident, &i.vis),
        syn::Item::Static(i) => (&i.ident, &i.vis),
        syn::Item::Mod(i) => (&i.ident, &i.vis),
        _ => return None,
    })
}

/// The attributes of an item (empty for the kinds scopes ignore).
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

impl ModuleScope {
    fn build(items: &[syn::Item], module_path: &[String]) -> Self {
        let mut scope = Self::default();
        // Items and imports the compiler drops by `#[cfg]` name nothing.
        let items: Vec<&syn::Item> = items
            .iter()
            .filter(|item| !super::cfg_eval::compiled_out(item_attrs(item)))
            .collect();
        for (ident, vis) in items.iter().filter_map(|item| item_ident(item)) {
            let name = ident_name(ident);
            if matches!(vis, syn::Visibility::Inherited) {
                scope.private.insert(name.clone());
            }
            scope
                .read_scopes
                .insert(name.clone(), visibility::read_scope(vis, module_path));
            scope.locals.insert(name);
        }
        for item in &items {
            let syn::Item::Use(item_use) = item else {
                continue;
            };
            let mut prefix = Vec::new();
            if item_use.leading_colon.is_some() {
                prefix.push(String::new());
            }
            // Collect this `use` into an empty map (`collect_use` never reads
            // the imports so far), then fold it in: the names it newly adds
            // are known without cloning every earlier key per `use`.
            let earlier = std::mem::take(&mut scope.imports);
            scope.collect_use(&item_use.tree, &mut prefix, module_path);
            let added = std::mem::replace(&mut scope.imports, earlier);
            let private = matches!(item_use.vis, syn::Visibility::Inherited);
            for (key, path) in added {
                if private && !scope.imports.contains_key(&key) {
                    scope.private.insert(key.clone());
                }
                scope.imports.insert(key, path);
            }
        }
        scope.reroot_chained_imports();
        scope
    }

    /// `use super::enums; use enums::Status;`: an import whose first segment
    /// names another import of this module goes through that import's path
    /// (repeated for chains; a self-referencing or cyclic chain stops).
    fn reroot_chained_imports(&mut self) {
        for _ in 0..=self.imports.len() {
            let mut changed = false;
            // Sorted: a cyclic chain resolves to a value that depends on the
            // order its keys are rerooted in, and `HashMap` order is random.
            let mut keys: Vec<String> = self.imports.keys().cloned().collect();
            keys.sort_unstable();
            for key in keys {
                let path = &self.imports[&key];
                // A head that is itself an import (not the name, not a root,
                // not an import of its own name) is replaced by its path.
                let Some(base) = path
                    .first()
                    .filter(|first| **first != key && !is_path_root(first))
                    .and_then(|first| {
                        self.imports
                            .get(first)
                            .filter(|base| base.first() != Some(first))
                    })
                else {
                    continue;
                };
                let rerooted: Vec<String> = base.iter().chain(&path[1..]).cloned().collect();
                self.imports.insert(key, rerooted);
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    fn collect_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>, module: &[String]) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(ident_name(&path.ident));
                self.collect_use(&path.tree, prefix, module);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                let ident = ident_name(&name.ident);
                if ident == "self" {
                    if let Some(last) = prefix.last().cloned() {
                        let path = normalize_use_path(prefix, module, &self.locals);
                        self.imports.insert(last, path);
                    }
                } else {
                    prefix.push(ident.clone());
                    let path = normalize_use_path(prefix, module, &self.locals);
                    prefix.pop();
                    self.imports.insert(ident, path);
                }
            }
            syn::UseTree::Rename(rename) => {
                let alias = ident_name(&rename.rename);
                if alias != "_" {
                    let ident = ident_name(&rename.ident);
                    if ident == "self" {
                        let path = normalize_use_path(prefix, module, &self.locals);
                        self.imports.insert(alias, path);
                    } else {
                        prefix.push(ident);
                        let path = normalize_use_path(prefix, module, &self.locals);
                        prefix.pop();
                        self.imports.insert(alias, path);
                    }
                }
            }
            syn::UseTree::Glob(_) => {
                self.globs
                    .push(normalize_use_path(prefix, module, &self.locals));
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.collect_use(item, prefix, module);
                }
            }
        }
    }

    /// The crate-wide path of `name` when this module (at `module_path`)
    /// imports or defines it.
    fn lookup(&self, name: &str, module_path: &[String]) -> Option<Vec<String>> {
        if let Some(import) = self.imports.get(name) {
            return Some(import.clone());
        }
        self.locals.contains(name).then(|| {
            module_path
                .iter()
                .cloned()
                .chain([name.to_string()])
                .collect()
        })
    }

    /// Whether the module glob-imports a `sea_orm` prelude (`DateTime`,
    /// `Date`, `Time`, `Json`, ... are then SeaORM's aliases).
    pub fn has_sea_orm_glob(&self) -> bool {
        self.globs.iter().any(|glob| {
            glob.iter()
                .find(|s| !s.is_empty())
                .is_some_and(|s| s == "sea_orm")
        })
    }
}

/// Re-root a path written inside `module` (absolute `crate::..` segments) so
/// it resolves from anywhere in the crate. External crate paths stay as
/// written; a leading `::` is kept as an empty first segment.
fn normalize_use_path(path: &[String], module: &[String], locals: &HashSet<String>) -> Vec<String> {
    match path.first().map(String::as_str) {
        Some("crate" | "") | None => path.to_vec(),
        Some("self") => module.iter().chain(&path[1..]).cloned().collect(),
        Some("super") => {
            let supers = path.iter().take_while(|s| *s == "super").count();
            let keep = module.len().saturating_sub(supers).max(1);
            module[..keep.min(module.len())]
                .iter()
                .chain(&path[supers..])
                .cloned()
                .collect()
        }
        Some(first) if locals.contains(first) && !module.is_empty() => {
            module.iter().chain(path).cloned().collect()
        }
        Some(_) => path.to_vec(),
    }
}

type ScopeCache = HashMap<
    (Option<super::crate_layout::CrateLayout>, Vec<String>),
    file_cache::Tracked<Option<Arc<ModuleScope>>>,
>;

thread_local! {
    static SCOPES: RefCell<ScopeCache> = RefCell::new(HashMap::new());
}

/// The scope of the module at `module_path` (absolute, `crate::..`). `None`
/// when the module is not found.
///
/// A scope holds only `String`s, so it is kept across macro invocations
/// while every file its lookup opened — or probed and found missing — is
/// unchanged (`file_cache::Tracked`, re-checked once per invocation): each
/// `schema_type!` otherwise re-parsed its source model's file, one more per
/// relation and one per glob target.
pub fn module_scope(module_path: &[String]) -> Option<Arc<ModuleScope>> {
    if module_path.first().is_none_or(|first| first != "crate") {
        return None;
    }
    let key = (super::crate_layout::crate_layout(), module_path.to_vec());
    let cached = SCOPES.with(|scopes| {
        scopes
            .borrow_mut()
            .get_mut(&key)
            .and_then(file_cache::Tracked::get)
    });
    if let Some(scope) = cached {
        return scope;
    }
    let entry = file_cache::Tracked::compute(|| {
        module_items(module_path).map(|items| Arc::new(ModuleScope::build(&items, module_path)))
    });
    let scope = entry.peek().clone();
    SCOPES.with(|scopes| {
        scopes.borrow_mut().insert(key, entry);
    });
    scope
}

/// What a name written inside a module refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A path usable from anywhere in the crate.
    Path(Vec<String>),
    /// Nothing in the module's scope names it (prelude / unknown).
    Unknown,
}

/// Resolve the path `segments`, written inside the module `module_path`.
///
/// `Unknown` is returned for single names the module neither defines nor
/// imports explicitly (std prelude types, glob imports that cannot be
/// pinned down) and for every name when the module cannot be located.
pub fn resolve_name(segments: &[String], module_path: &[String]) -> Resolved {
    let Some(first) = segments.first() else {
        return Resolved::Unknown;
    };
    match first.as_str() {
        "crate" | "" => return Resolved::Path(segments.to_vec()),
        "self" | "super" if module_path.first().is_some_and(|s| s == "crate") => {
            return Resolved::Path(relative_path(segments, module_path));
        }
        _ => {}
    }
    let Some(scope) = module_scope(module_path) else {
        return Resolved::Unknown;
    };
    let rest = segments[1..].iter().cloned();
    if let Some(mut path) = scope.lookup(first, module_path) {
        // `use enums::Status;` where `enums` came in through a glob.
        if let Some(head) = path.first().filter(|head| !is_path_root(head))
            && !scope.imports.contains_key(head)
            && !scope.locals.contains(head)
            && let Some(base) = glob_lookup(&scope, head, module_path)
        {
            path = base.into_iter().chain(path.into_iter().skip(1)).collect();
        }
        return Resolved::Path(path.into_iter().chain(rest).collect());
    }
    // Crate-internal glob imports: the first glob module naming `first`
    // (`kinds::Kind` through `use super::*;` too).
    glob_lookup(&scope, first, module_path).map_or(Resolved::Unknown, |path| {
        Resolved::Path(path.into_iter().chain(rest).collect())
    })
}

/// `self::X` / `super::X..` written inside `module_path`: the module the
/// relative prefix names, then `X` as that module's own scope sees it. An
/// import there resolves to its use target (`self::Status` for a private
/// `use super::enums::Status;` is `crate::models::enums::Status`, the path
/// `memo::Status` would be E0603); a local item, or a name the module does
/// not know, keeps the normalized module path.
fn relative_path(segments: &[String], module_path: &[String]) -> Vec<String> {
    let prefix = segments
        .iter()
        .take_while(|s| *s == "self" || *s == "super")
        .count();
    let target = normalize_use_path(&segments[..prefix], module_path, &HashSet::new());
    let Some((name, tail)) = segments[prefix..].split_first() else {
        return target;
    };
    module_scope(&target)
        // A glob import of the target names it too (R30-SM3-L1): `self::X`
        // is the target's own scope, globs included, as `resolve_name` sees it.
        .and_then(|scope| {
            scope
                .lookup(name, &target)
                .or_else(|| glob_lookup(&scope, name, &target))
        })
        .unwrap_or_else(|| target.iter().chain([name]).cloned().collect())
        .into_iter()
        .chain(tail.iter().cloned())
        .collect()
}

/// A path root that never names an import.
fn is_path_root(segment: &str) -> bool {
    matches!(segment, "crate" | "" | "self" | "super")
}

/// `name` through the crate-internal glob imports of `scope` (the module at
/// `module_path`): the first glob module that imports or defines it, skipping
/// a private name unless `module_path` lies inside that glob module (a glob
/// import only brings in the names visible to the importer).
fn glob_lookup(scope: &ModuleScope, name: &str, module_path: &[String]) -> Option<Vec<String>> {
    glob_lookup_through(scope, name, module_path, &mut Vec::new())
}

/// [`glob_lookup`] with the glob modules already being searched (`chain`):
/// a glob target that itself glob-re-exports the name (`pub use self::types::*;`
/// in the target, R23-SM3-01) is searched one level further, and a glob
/// already on the chain is skipped so a cycle of globs ends.
fn glob_lookup_through(
    scope: &ModuleScope,
    name: &str,
    module_path: &[String],
    chain: &mut Vec<Vec<String>>,
) -> Option<Vec<String>> {
    for glob in &scope.globs {
        if glob.first().is_none_or(|s| s != "crate") || chain.contains(glob) {
            continue;
        }
        let Some(target) = module_scope(glob) else {
            continue;
        };
        if target.private.contains(name) && !module_path.starts_with(glob) {
            continue;
        }
        let found = target.lookup(name, glob).or_else(|| {
            chain.push(glob.clone());
            let nested = glob_lookup_through(&target, name, module_path, chain);
            chain.pop();
            nested
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// `[crate, a, B]` -> `crate::a::B` (`""` first = leading `::`).
pub fn path_tokens(segments: &[String]) -> TokenStream {
    let (leading, rest) = match segments.split_first() {
        Some((first, rest)) if first.is_empty() => (quote!(::), rest),
        _ => (TokenStream::new(), segments),
    };
    let idents = rest.iter().map(|segment| segment_ident(segment));
    quote! { #leading #(#idents)::* }
}

/// An identifier for a path segment; keywords other than the path roots
/// become raw identifiers.
pub fn segment_ident(segment: &str) -> syn::Ident {
    let span = proc_macro2::Span::call_site();
    let raw = segment.strip_prefix("r#").unwrap_or(segment);
    if matches!(raw, "crate" | "self" | "super" | "Self") {
        return syn::Ident::new(raw, span);
    }
    // A plain ASCII identifier (the common case, once per emitted segment) is
    // built directly; anything else goes through syn's parser as before.
    let plain = raw != "_"
        && raw.bytes().next().is_some_and(|b| !b.is_ascii_digit())
        && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if plain {
        return if is_keyword(raw) {
            syn::Ident::new_raw(raw, span)
        } else {
            syn::Ident::new(raw, span)
        };
    }
    syn::parse_str::<syn::Ident>(raw).unwrap_or_else(|_| syn::Ident::new_raw(raw, span))
}

/// The words `syn` refuses to parse as an identifier (its
/// `accept_as_ident`), `_` excepted, plus `gen` (reserved in edition 2024 —
/// `r#gen` is valid in every edition, so it is always written raw).
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "abstract"
            | "as"
            | "async"
            | "await"
            | "become"
            | "box"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "final"
            | "fn"
            | "for"
            | "gen"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "macro"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "override"
            | "priv"
            | "pub"
            | "ref"
            | "return"
            | "Self"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "unsafe"
            | "unsized"
            | "use"
            | "virtual"
            | "where"
            | "while"
            | "yield"
    )
}

#[cfg(test)]
mod tests;
