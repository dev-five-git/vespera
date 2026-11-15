//! Which items exist only in `cfg(test)` builds.
//!
//! `cargo build`, `cargo test` (the `--test` harness build) and
//! rust-analyzer (which enables `cfg(test)` for workspace crates) all expand
//! the same `vespera!` / `export_app!` and all write the same user-visible
//! files (`openapi.json`, the exported `<App>.openapi.json`).  `#[route]` /
//! `#[derive(Schema)]` items behind `#[cfg(test)]` only register themselves
//! in the test builds, so the documents flipped with whichever build ran
//! last.  The production router never serves such an item, so the document
//! leaves it out in every build: the router still mounts a test-only route
//! (tests call it), the OpenAPI document does not describe it.
//!
//! An item is test-only when a `#[cfg(..)]` predicate that can only hold
//! under `cfg(test)` (`test`, `all(.., test, ..)`, `any` of such) gates it,
//! an enclosing scope (inline module, fn / impl / trait / const / static
//! body, item or statement macro), or a `mod` declaration on the module chain
//! from the crate root (`src/lib.rs` / `src/main.rs`) to its file.
//! Predicates over features etc. are not interpreted: every build of one
//! cargo invocation agrees on them.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::{Item, punctuated::Punctuated};

use crate::{metadata::CollectedMetadata, schema_macro::file_cache};

/// Whether the `#[cfg(..)]` predicate `predicate` (token text) can only hold
/// in a `cfg(test)` build.
pub fn is_test_only_predicate(predicate: &str) -> bool {
    syn::parse_str::<syn::Meta>(predicate).is_ok_and(|meta| meta_test_only(&meta))
}

fn meta_test_only(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let nested = list
                .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .unwrap_or_default();
            if list.path.is_ident("all") {
                nested.iter().any(meta_test_only)
            } else if list.path.is_ident("any") {
                !nested.is_empty() && nested.iter().all(meta_test_only)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

/// Whether `attrs` hold a test-only `#[cfg(..)]`.
pub fn attrs_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .filter_map(|attr| attr.meta.require_list().ok())
        .any(|list| is_test_only_predicate(&list.tokens.to_string()))
}

/// Whether `content` could hold a test-only `#[cfg(..)]` — a cheap text
/// filter that spares the parse of (almost) every file.
fn may_gate_tests(content: &str) -> bool {
    content.contains("cfg") && content.contains("test")
}

/// Test-only classification for one crate, memoizing each declaring file's
/// test-gated `mod` names.
pub struct TestScope {
    /// Canonical `<manifest>/src`, when known.
    src_dir: Option<PathBuf>,
    /// Declaring file → names of the `mod name;` declarations it gates.
    gated_mods: HashMap<PathBuf, HashSet<String>>,
}

impl TestScope {
    /// The scope of the crate whose sources live in `src_dir`.
    pub fn new(src_dir: Option<&Path>) -> Self {
        Self {
            src_dir: src_dir.and_then(file_cache::canonicalize_cached),
            gated_mods: HashMap::new(),
        }
    }

    /// The scope of the crate being expanded (`CARGO_MANIFEST_DIR/src`).
    pub fn current() -> Self {
        let src_dir = crate::env_access::var("CARGO_MANIFEST_DIR")
            .ok()
            .map(|dir| PathBuf::from(dir).join("src"));
        Self::new(src_dir.as_deref())
    }

    /// Whether `file` joins the crate only through a test-gated `mod`
    /// declaration (any link of the chain from `src/lib.rs` / `src/main.rs`).
    /// A file outside `src/` is never classified.
    pub fn file_test_only(&mut self, file: &Path) -> bool {
        let Some(src_dir) = self.src_dir.clone() else {
            return false;
        };
        let Some(segments) =
            file_cache::canonicalize_cached(file).and_then(|file| module_segments(&src_dir, &file))
        else {
            return false;
        };
        let mut parent = src_dir.clone();
        for (depth, segment) in segments.iter().enumerate() {
            let declarers = if depth == 0 {
                vec![src_dir.join("lib.rs"), src_dir.join("main.rs")]
            } else {
                vec![parent.join("mod.rs"), parent.with_extension("rs")]
            };
            if declarers
                .iter()
                .any(|declarer| self.gated_mods_of(declarer).contains(segment))
            {
                return true;
            }
            parent.push(segment);
        }
        false
    }

    fn gated_mods_of(&mut self, declarer: &Path) -> &HashSet<String> {
        /// A declarer's test-gated `mod` names, kept while it is unchanged.
        struct GatedMods(HashSet<String>);
        self.gated_mods
            .entry(declarer.to_path_buf())
            .or_insert_with(|| {
                let gated = file_cache::derived(declarer, |content| {
                    GatedMods(gating_names(content, |item| match item {
                        Item::Mod(module) if attrs_test_only(&module.attrs) => Some(&module.ident),
                        _ => None,
                    }))
                });
                gated.map(|gated| gated.0.clone()).unwrap_or_default()
            })
    }
}

/// The unraw names `pick` selects among the top-level items of `content`,
/// when its text could gate anything on `cfg(test)` (empty otherwise or
/// when it does not parse).  Only `String`s leave, so the result can be
/// cached across invocations (`file_cache::derived`).
fn gating_names(content: &str, pick: impl Fn(&Item) -> Option<&syn::Ident>) -> HashSet<String> {
    if !may_gate_tests(content) {
        return HashSet::new();
    }
    syn::parse_file(content)
        .map(|ast| {
            ast.items
                .iter()
                .filter_map(|item| pick(item))
                .map(|ident| syn::ext::IdentExt::unraw(ident).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// `src/a/b.rs` / `src/a/b/mod.rs` → `["a", "b"]`; a crate root → `[]`.
fn module_segments(src_dir: &Path, file: &Path) -> Option<Vec<String>> {
    let relative = file.strip_prefix(src_dir).ok()?;
    let mut segments: Vec<String> = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segments.len() == 1 && matches!(segments[0].as_str(), "lib" | "main") {
        segments.clear();
    }
    if segments.last().is_some_and(|last| last == "mod") {
        segments.pop();
    }
    Some(segments)
}

/// Top-level functions of `file` behind a test-only `#[cfg(..)]` (kept while
/// the file is unchanged).
pub fn test_only_fns(file: &Path) -> HashSet<String> {
    struct TestFns(HashSet<String>);
    file_cache::derived(file, |content| {
        TestFns(gating_names(content, |item| match item {
            Item::Fn(function) if attrs_test_only(&function.attrs) => Some(&function.sig.ident),
            _ => None,
        }))
    })
    .map(|fns| fns.0.clone())
    .unwrap_or_default()
}

/// The test-only items of one file: the `(name, line)` of every struct /
/// enum gated by a test-only `#[cfg(..)]` on itself or an enclosing scope,
/// and the line range of every other gated item or statement (an inline
/// module, a fn / impl / trait / const / static, an item or statement macro
/// such as `#[cfg(test)] schema_type!(..)`) — whatever it nests is
/// test-only too (a `#[derive(Schema)]` inside a `#[test] fn`, a
/// `schema_type!` inside `mod tests`).
#[derive(Default)]
struct GatedItems {
    items: HashSet<(String, usize)>,
    ranges: Vec<(usize, usize)>,
}

impl GatedItems {
    fn contains(&self, ident: &str, line: usize) -> bool {
        self.items.contains(&(ident.to_owned(), line))
            || self
                .ranges
                .iter()
                .any(|&(start, end)| (start..=end).contains(&line))
    }
}

/// `file`'s [`GatedItems`] (empty for an unreadable file), kept while the
/// file is unchanged (`file_cache::derived`): `vespera!` asks for every
/// schema file on every invocation, route-cache hits included.
fn gated_items(file: &Path) -> std::rc::Rc<GatedItems> {
    file_cache::derived(file, |content| {
        let mut gated = GatedItems::default();
        if may_gate_tests(content) {
            // Only fallback spans carry real line numbers (see `cron_impl`);
            // nothing parsed here leaves the closure.
            file_cache::with_fallback_parse(content, |ast| {
                let mut collector = GatedCollector {
                    gated: false,
                    out: &mut gated,
                };
                syn::visit::Visit::visit_file(&mut collector, ast);
            });
        }
        gated
    })
    .unwrap_or_default()
}

/// Walks a file carrying "inside a test-only scope" through every nested
/// item, impl / trait item and statement.
struct GatedCollector<'a> {
    gated: bool,
    out: &'a mut GatedItems,
}

impl GatedCollector<'_> {
    /// Visit `node` gated when its `attrs` or an ancestor gate it; the
    /// outermost gated node records its line range.
    fn scoped<T: syn::spanned::Spanned>(
        &mut self,
        attrs: &[syn::Attribute],
        node: &T,
        visit: impl FnOnce(&mut Self),
    ) {
        let outer = self.gated;
        if !outer && attrs_test_only(attrs) {
            let span = node.span();
            self.out.ranges.push((span.start().line, span.end().line));
            self.gated = true;
        }
        visit(self);
        self.gated = outer;
    }
}

/// The attributes of the items that can nest a schema.
fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        _ => &[],
    }
}

impl<'ast> syn::visit::Visit<'ast> for GatedCollector<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if let Item::Struct(syn::ItemStruct { attrs, ident, .. })
        | Item::Enum(syn::ItemEnum { attrs, ident, .. }) = item
        {
            if self.gated || attrs_test_only(attrs) {
                self.out
                    .items
                    .insert((ident.to_string(), ident.span().start().line));
            }
            return;
        }
        self.scoped(item_attrs(item), item, |this| {
            syn::visit::visit_item(this, item);
        });
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Const(item) => &item.attrs,
            _ => &[],
        };
        self.scoped(attrs, item, |this| syn::visit::visit_impl_item(this, item));
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::TraitItem::Fn(item) => &item.attrs,
            _ => &[],
        };
        self.scoped(attrs, item, |this| syn::visit::visit_trait_item(this, item));
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let attrs: &[syn::Attribute] = match stmt {
            syn::Stmt::Macro(stmt) => &stmt.attrs,
            syn::Stmt::Local(stmt) => &stmt.attrs,
            _ => &[],
        };
        self.scoped(attrs, stmt, |this| syn::visit::visit_stmt(this, stmt));
    }
}

/// `(file, ident, line)` of a `#[derive(Schema)]` source identity
/// (`<file>::<Ident>@<line>:<column>`, see `schema_impl`).
fn parse_identity(identity: &str) -> Option<(&str, &str, usize)> {
    let (path_ident, position) = identity.rsplit_once('@')?;
    let (file, ident) = path_ident.rsplit_once("::")?;
    let line = position.split_once(':')?.0.parse().ok()?;
    Some((file, ident, line))
}

impl TestScope {
    /// Whether the schema with `identity` is test-only (its file or the item
    /// itself).
    fn schema_test_only(
        &mut self,
        identity: &str,
        items: &mut HashMap<String, std::rc::Rc<GatedItems>>,
    ) -> bool {
        let Some((file, ident, line)) = parse_identity(identity) else {
            return false;
        };
        self.file_test_only(Path::new(file))
            || items
                .entry(file.to_owned())
                .or_insert_with(|| gated_items(Path::new(file)))
                .contains(ident, line)
    }
}

/// Hash of every test-only verdict the OpenAPI document depends on: each
/// documented schema's source identity + verdict and each stored route
/// file's declarer-chain verdict.  The verdicts come from files outside the
/// route folder (`src/lib.rs`, `src/routes.rs`, a schema's own file), which
/// the route-file fingerprints never cover, so the route cache folds this in:
/// gating `mod fixtures;` on `cfg(test)` must invalidate every build
/// variant's cache, not only the one whose storage changed.
pub fn verdict_hash(
    schemas: &HashMap<String, crate::metadata::StructMetadata>,
    routes: &[crate::route_impl::StoredRouteInfo],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut scope = TestScope::current();
    let mut items = HashMap::new();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut identities: Vec<&str> = schemas
        .values()
        .filter(|schema| schema.include_in_openapi)
        .filter_map(|schema| schema.source_identity.as_deref())
        .collect();
    identities.sort_unstable();
    identities.dedup();
    for identity in identities {
        (identity, scope.schema_test_only(identity, &mut items)).hash(&mut hasher);
    }
    let mut files: Vec<&str> = routes
        .iter()
        .filter_map(|route| route.file_path.as_deref())
        .collect();
    files.sort_unstable();
    files.dedup();
    for file in files {
        (file, scope.file_test_only(Path::new(file))).hash(&mut hasher);
    }
    hasher.finish()
}

/// `metadata` without its test-only routes and schemas — what the OpenAPI
/// document describes in every build.  Borrows when there is none.
pub fn documented_view(metadata: &CollectedMetadata) -> Cow<'_, CollectedMetadata> {
    let mut scope = TestScope::current();
    let mut items = HashMap::new();
    let test_structs: Vec<bool> = metadata
        .structs
        .iter()
        .map(|schema| {
            schema.include_in_openapi
                && schema
                    .source_identity
                    .as_deref()
                    .is_some_and(|identity| scope.schema_test_only(identity, &mut items))
        })
        .collect();
    if metadata.test_only_routes.is_empty() && !test_structs.contains(&true) {
        return Cow::Borrowed(metadata);
    }
    let mut view = metadata.clone();
    view.routes.retain(|route| {
        !metadata.test_only_routes.iter().any(|test_only| {
            test_only.file_path == route.file_path && test_only.function_name == route.function_name
        })
    });
    let mut test_structs = test_structs.into_iter();
    view.structs
        .retain(|_| !test_structs.next().unwrap_or_default());
    Cow::Owned(view)
}

#[cfg(test)]
mod tests;
