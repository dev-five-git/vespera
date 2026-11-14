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
//!   (`src/lib.rs` / `src/main.rs`) through `mod x;` declarations — honouring
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

fn src_dir() -> Option<PathBuf> {
    file_cache::get_manifest_dir().map(|dir| Path::new(&dir).join("src"))
}

/// Crate root candidates: `main.rs` first while compiling a binary target.
pub(super) fn crate_root_files(src: &Path) -> [PathBuf; 2] {
    let (lib, main) = (src.join("lib.rs"), src.join("main.rs"));
    if crate::env_access::var("CARGO_BIN_NAME").is_ok() {
        [main, lib]
    } else {
        [lib, main]
    }
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

/// Step from `cursor` into its child module `segment`.
fn descend(cursor: Cursor, segment: &str) -> Option<Cursor> {
    let module = cursor.items.iter().find_map(|item| match item {
        syn::Item::Mod(module) if ident_name(&module.ident) == segment => Some(module),
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

/// The items of the module at `module_path` (`crate::a::b` or `a::b`).
pub fn module_items(module_path: &[String]) -> Option<Vec<syn::Item>> {
    let segments = crate_relative(module_path)?;
    let src = src_dir()?;
    // Fast path: the longest prefix that is a module file of its own, then
    // inline / nested modules below it. Works for trees without a crate root
    // and costs no root walk for the common `src/a/b.rs` layout.
    for split in (1..=segments.len()).rev() {
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
    crate_root_files(&src)
        .iter()
        .find_map(|root| descend_all(open_file(root, true)?, segments))
        .map(|cursor| cursor.items)
}

fn descend_all(mut cursor: Cursor, segments: &[String]) -> Option<Cursor> {
    for segment in segments {
        cursor = descend(cursor, segment)?;
    }
    Some(cursor)
}

/// Token-string definition of the top-level struct `name` in `items`.
pub fn struct_in_items(items: &[syn::Item], name: &str) -> Option<String> {
    items.iter().find_map(|item| match item {
        syn::Item::Struct(item) if ident_name(&item.ident) == name => {
            Some(quote!(#item).to_string())
        }
        _ => None,
    })
}

/// The struct `name` defined anywhere in `items` (inline modules included)
/// when exactly one such struct exists, with the inline module chain.
pub fn unique_nested_struct(items: &[syn::Item], name: &str) -> Option<(String, Vec<String>)> {
    fn walk(
        items: &[syn::Item],
        name: &str,
        chain: &mut Vec<String>,
        found: &mut Vec<(String, Vec<String>)>,
    ) {
        for item in items {
            match item {
                syn::Item::Struct(item) if ident_name(&item.ident) == name => {
                    found.push((quote!(#item).to_string(), chain.clone()));
                }
                syn::Item::Mod(module) => {
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
}

impl ModuleScope {
    fn build(items: &[syn::Item], module_path: &[String]) -> Self {
        let mut scope = Self::default();
        for item in items {
            let ident = match item {
                syn::Item::Struct(i) => Some(&i.ident),
                syn::Item::Enum(i) => Some(&i.ident),
                syn::Item::Union(i) => Some(&i.ident),
                syn::Item::Type(i) => Some(&i.ident),
                syn::Item::Trait(i) => Some(&i.ident),
                syn::Item::Fn(i) => Some(&i.sig.ident),
                syn::Item::Const(i) => Some(&i.ident),
                syn::Item::Static(i) => Some(&i.ident),
                syn::Item::Mod(i) => Some(&i.ident),
                _ => None,
            };
            scope.locals.extend(ident.map(ident_name));
        }
        let uses: Vec<&syn::ItemUse> = items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Use(item_use) => Some(item_use),
                _ => None,
            })
            .collect();
        for item_use in uses {
            let mut prefix = Vec::new();
            if item_use.leading_colon.is_some() {
                prefix.push(String::new());
            }
            scope.collect_use(&item_use.tree, &mut prefix, module_path);
        }
        scope
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
    HashMap<(Option<String>, Vec<String>), Option<Arc<ModuleScope>>>,
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
    let key = (file_cache::get_manifest_dir(), module_path.to_vec());
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
    if let Some(path) = scope.lookup(first, module_path) {
        return Resolved::Path(
            path.into_iter()
                .chain(segments[1..].iter().cloned())
                .collect(),
        );
    }
    if segments.len() > 1 {
        return Resolved::Unknown;
    }
    // Crate-internal glob imports: the first glob module naming `first`.
    scope
        .globs
        .iter()
        .filter(|glob| glob.first().is_some_and(|s| s == "crate"))
        .find_map(|glob| module_scope(glob)?.lookup(first, glob))
        .map_or(Resolved::Unknown, Resolved::Path)
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
