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

/// Items of one module plus the location state needed to descend further.
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
}

fn open_file(path: &Path, force_mod_rs: bool) -> Option<Cursor> {
    let file = file_cache::get_parsed_file(path)?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let mod_rs = force_mod_rs || matches!(stem.as_str(), "mod" | "lib" | "main");
    Some(Cursor {
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
                && !super::cfg_eval::statically_inactive(&module.attrs) =>
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
/// `None` for relative (`self` / `super`) paths.
fn crate_relative(module_path: &[String]) -> Option<&[String]> {
    let rest = match module_path.first().map(String::as_str) {
        Some("crate") => &module_path[1..],
        _ => module_path,
    };
    (!rest
        .iter()
        .any(|s| s == "self" || s == "super" || s == "crate"))
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
    let redirected = |file: &Path, segment: &str| {
        file_cache::get_parsed_file(file).is_some_and(|file| {
            file.items.iter().any(|item| {
                matches!(item, syn::Item::Mod(module) if ident_name(&module.ident) == segment
                    && (module.content.is_some() || path_attr(&module.attrs).is_some()))
            })
        })
    };
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
            if ident_name(&item.ident) == name
                && !super::cfg_eval::statically_inactive(&item.attrs) =>
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
pub fn item_module_chain(items: &[syn::Item], name: &str, line: usize) -> Vec<String> {
    pick_chain(chain_candidates(items, name), line)
}

/// [`item_module_chain`] for `file`, whose cached parse is `items`.  A repeated
/// name is told apart by line, so the file is then re-parsed with
/// proc-macro2's fallback forced: inside the compiler every token of re-parsed
/// text carries the call site's span, so no candidate's line would match.
pub fn item_module_chain_in_file(
    file: &Path,
    items: &[syn::Item],
    name: &str,
    line: usize,
) -> Vec<String> {
    let found = chain_candidates(items, name);
    if found.len() <= 1 {
        return pick_chain(found, line);
    }
    with_line_spans(file, |parsed| item_module_chain(&parsed.items, name, line)).unwrap_or_default()
}

/// Run `read` over `file` re-parsed with proc-macro2's fallback forced, so
/// spans carry real line numbers (inside the compiler every token of
/// re-parsed text carries the call site's span).  Only `read`'s result
/// leaves: no fallback token is mixed with compiler tokens.
fn with_line_spans<R>(file: &Path, read: impl FnOnce(&syn::File) -> R) -> Option<R> {
    let content = file_cache::get_file_content(file)?;
    proc_macro2::fallback::force();
    let _unforce = Unforce;
    syn::parse_file(&content).ok().map(|parsed| read(&parsed))
}

/// Restores proc-macro2's normal mode even if parsing panics.
struct Unforce;

impl Drop for Unforce {
    fn drop(&mut self) {
        proc_macro2::fallback::unforce();
    }
}

/// The inline `mod` chain whose braces enclose `line` of `file` — where a
/// macro invoked on that line resolves its names (empty at the file's top
/// level, for line `0` or an unreadable file).
pub fn inline_chain_at(file: &Path, line: usize) -> Vec<String> {
    fn enclosing(items: &[syn::Item], line: usize, chain: &mut Vec<String>) {
        for item in items {
            if let syn::Item::Mod(syn::ItemMod {
                ident,
                content: Some((brace, inner)),
                ..
            }) = item
                && (brace.span.open().start().line..=brace.span.close().end().line).contains(&line)
            {
                chain.push(ident_name(ident));
                enclosing(inner, line, chain);
                return;
            }
        }
    }
    if line == 0
        || !file_cache::get_file_content(file).is_some_and(|content| content.contains("mod"))
    {
        return Vec::new();
    }
    with_line_spans(file, |parsed| {
        let mut chain = Vec::new();
        enclosing(&parsed.items, line, &mut chain);
        chain
    })
    .unwrap_or_default()
}

/// The only candidate's chain, or (several) the one whose ident is at `line`.
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
fn chain_candidates(items: &[syn::Item], name: &str) -> Vec<(usize, Vec<String>)> {
    fn walk(
        items: &[syn::Item],
        name: &str,
        chain: &mut Vec<String>,
        found: &mut Vec<(usize, Vec<String>)>,
    ) {
        for item in items {
            let ident = match item {
                syn::Item::Struct(item) => &item.ident,
                syn::Item::Enum(item) => &item.ident,
                syn::Item::Mod(module) => {
                    if let Some((_, inner)) = &module.content {
                        chain.push(ident_name(&module.ident));
                        walk(inner, name, chain, found);
                        chain.pop();
                    }
                    continue;
                }
                _ => continue,
            };
            if ident_name(ident) == name {
                found.push((ident.span().start().line, chain.clone()));
            }
        }
    }
    let mut found = Vec::new();
    walk(items, name, &mut Vec::new(), &mut found);
    found
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
                        && !super::cfg_eval::statically_inactive(&item.attrs) =>
                {
                    found.push((quote!(#item).to_string(), chain.clone()));
                }
                syn::Item::Mod(module) if !super::cfg_eval::statically_inactive(&module.attrs) => {
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
            .filter(|item| !super::cfg_eval::statically_inactive(item_attrs(item)))
            .collect();
        for (ident, vis) in items.iter().filter_map(|item| item_ident(item)) {
            let name = ident_name(ident);
            if matches!(vis, syn::Visibility::Inherited) {
                scope.private.insert(name.clone());
            }
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
            let before: HashSet<String> = scope.imports.keys().cloned().collect();
            scope.collect_use(&item_use.tree, &mut prefix, module_path);
            if matches!(item_use.vis, syn::Visibility::Inherited) {
                let added: Vec<String> = scope
                    .imports
                    .keys()
                    .filter(|key| !before.contains(*key))
                    .cloned()
                    .collect();
                scope.private.extend(added);
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
            let keys: Vec<String> = self.imports.keys().cloned().collect();
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

type ScopeCache = (
    u64,
    HashMap<(Option<PathBuf>, Vec<String>), Option<Arc<ModuleScope>>>,
);

thread_local! {
    static SCOPES: RefCell<ScopeCache> = RefCell::new((u64::MAX, HashMap::new()));
}

/// The scope of the module at `module_path` (absolute, `crate::..`), cached
/// for the current macro invocation. `None` when the module is not found.
pub fn module_scope(module_path: &[String]) -> Option<Arc<ModuleScope>> {
    if module_path.first().is_none_or(|first| first != "crate") {
        return None;
    }
    let epoch = file_cache::current_epoch();
    let key = (
        super::crate_layout::crate_layout().map(|layout| layout.module_dir),
        module_path.to_vec(),
    );
    let cached = SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        if scopes.0 != epoch {
            *scopes = (epoch, HashMap::new());
        }
        scopes.1.get(&key).cloned()
    });
    if let Some(scope) = cached {
        return scope;
    }
    let scope =
        module_items(module_path).map(|items| Arc::new(ModuleScope::build(&items, module_path)));
    SCOPES.with(|scopes| {
        scopes.borrow_mut().1.insert(key, scope.clone());
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
            return Resolved::Path(normalize_use_path(segments, module_path, &HashSet::new()));
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

/// A path root that never names an import.
fn is_path_root(segment: &str) -> bool {
    matches!(segment, "crate" | "" | "self" | "super")
}

/// `name` through the crate-internal glob imports of `scope` (the module at
/// `module_path`): the first glob module that imports or defines it, skipping
/// a private name unless `module_path` lies inside that glob module (a glob
/// import only brings in the names visible to the importer).
fn glob_lookup(scope: &ModuleScope, name: &str, module_path: &[String]) -> Option<Vec<String>> {
    scope
        .globs
        .iter()
        .filter(|glob| glob.first().is_some_and(|s| s == "crate"))
        .find_map(|glob| {
            let target = module_scope(glob)?;
            if target.private.contains(name) && !module_path.starts_with(glob) {
                return None;
            }
            target.lookup(name, glob)
        })
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
    syn::parse_str::<syn::Ident>(raw).unwrap_or_else(|_| syn::Ident::new_raw(raw, span))
}

#[cfg(test)]
mod tests;
