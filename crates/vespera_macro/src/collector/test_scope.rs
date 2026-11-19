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
//! body, item or statement macro, or a `#[test]` / `#[tokio::test]` fn, which
//! only `rustc --test` compiles), or a `mod` declaration on the module chain
//! from the crate root (`src/lib.rs` / `src/main.rs`) to its file.
//! Predicates over features etc. are not interpreted: every build of one
//! cargo invocation agrees on them.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::{Item, punctuated::Punctuated};

use super::module_scope::sibling_rs_file;
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

/// Whether `attrs` mark a test harness fn (`#[test]`, `#[tokio::test]`, any
/// `<path>::test`): rustc compiles one only under `--test`, so its body is as
/// test-only as a `#[cfg(test)]` one.
fn attrs_test_harness(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
    })
}

/// Whether `content` could hold a test-only `#[cfg(..)]` — a cheap text
/// filter that spares the parse of (almost) every file.
fn may_gate_tests(content: &str) -> bool {
    content.contains("cfg") && content.contains("test")
}

/// Test-only classification for one crate, memoizing each declaring file's
/// test-gated `mod` names.
pub struct TestScope {
    /// `<manifest>/src` folded lexically (the logical path rustc and the
    /// route walker see), when known.
    src_dir: Option<PathBuf>,
    /// Canonical `<manifest>/src`, the fallback for a file whose logical
    /// path does not lie under `src_dir` (a different spelling of it).
    canonical_src_dir: Option<PathBuf>,
    /// Declaring file → its test-gated `mod` declarations.
    gated_mods: HashMap<PathBuf, std::rc::Rc<GatedMods>>,
    /// The files the `#[path]` modules of `src/` name, found by one scan of
    /// the crate (`None` until a file needs it).
    path_targets: Option<PathTargets>,
}

/// The files named by `#[path]` modules of a crate, by whether every build
/// loads them (`open`) or only `cfg(test)` builds do (`gated`).
#[derive(Default)]
struct PathTargets {
    gated: Vec<PathBuf>,
    open: Vec<PathBuf>,
}

impl TestScope {
    /// The scope of the crate whose sources live in `src_dir`.
    pub fn new(src_dir: Option<&Path>) -> Self {
        Self {
            src_dir: src_dir.and_then(lexical),
            canonical_src_dir: src_dir.and_then(file_cache::canonicalize_cached),
            gated_mods: HashMap::new(),
            path_targets: None,
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
    ///
    /// The chain follows the file's logical path (`.` / `..` folded without
    /// resolving symlinks), as rustc's module tree does: a
    /// `#[cfg(test)] mod fixtures;` whose directory is a symlink leading out
    /// of `src/` still gates it. Only a path that is not lexically under
    /// `src/` falls back to the canonical one.
    pub fn file_test_only(&mut self, file: &Path) -> bool {
        let lexical_chain = self.src_dir.clone().and_then(|src_dir| {
            let segments = module_segments(&src_dir, &lexical(file)?)?;
            Some((src_dir, segments))
        });
        let Some((src_dir, segments)) = lexical_chain.or_else(|| {
            let src_dir = self.canonical_src_dir.clone()?;
            let file = file_cache::canonicalize_cached(file)?;
            let segments = module_segments(&src_dir, &file)?;
            Some((src_dir, segments))
        }) else {
            return false;
        };
        let mut parent = src_dir.clone();
        for (depth, segment) in segments.iter().enumerate() {
            let declarers = if depth == 0 {
                vec![src_dir.join("lib.rs"), src_dir.join("main.rs")]
            } else {
                vec![parent.join("mod.rs"), sibling_rs_file(&parent)]
            };
            let rest = &segments[depth..];
            if declarers.iter().any(|declarer| {
                self.gated_mods_of(declarer)
                    .chains
                    .iter()
                    .any(|gated| chain_starts_with(rest, gated))
            }) {
                // A same-named ungated declaration loads the file in every
                // build, so a gated twin next to it does not make it test-only.
                return !self.open_chain_loads(&src_dir, &segments);
            }
            parent.push(segment);
        }
        self.file_path_gated(file, &src_dir, &segments)
    }

    /// Whether a test-gated `#[path = ".."]` module names `file` and no
    /// ungated declaration loads it (`#[cfg(test)] #[path = "m.rs"] mod t;`
    /// next to `mod m;` leaves `m.rs` in every build). Such a module carries
    /// no name the chain could match, so the file is found by its path (or
    /// by the directory of a non-`mod.rs` module's own file, whose child
    /// modules sit beside it).
    fn file_path_gated(&mut self, file: &Path, src_dir: &Path, segments: &[String]) -> bool {
        lexical(file).is_some_and(|file| {
            let loads = |targets: &[PathBuf]| {
                targets
                    .iter()
                    .any(|target| file == *target || file.starts_with(child_dir(target)))
            };
            let targets = self.path_targets();
            let gated = loads(&targets.gated);
            let open = loads(&targets.open);
            gated && !open && !self.open_chain_loads(src_dir, segments)
        })
    }

    /// Whether an ungated `mod` declaration on the module chain of `segments`
    /// loads the file (the chain's own name, not a `#[path]`).
    fn open_chain_loads(&mut self, src_dir: &Path, segments: &[String]) -> bool {
        let mut parent = src_dir.to_path_buf();
        for (depth, segment) in segments.iter().enumerate() {
            let declarers = if depth == 0 {
                vec![src_dir.join("lib.rs"), src_dir.join("main.rs")]
            } else {
                vec![parent.join("mod.rs"), sibling_rs_file(&parent)]
            };
            let rest = &segments[depth..];
            if declarers.iter().any(|declarer| {
                self.gated_mods_of(declarer)
                    .open_chains
                    .iter()
                    .any(|open| chain_is(rest, open))
            }) {
                return true;
            }
            parent.push(segment);
        }
        false
    }

    /// The `#[path]` files of every module under `src/`, from one scan of
    /// the crate (memoized per scope).
    fn path_targets(&mut self) -> &PathTargets {
        if self.path_targets.is_none() {
            let files: Vec<PathBuf> = self
                .src_dir
                .as_deref()
                .map(|dir| file_cache::list_rs_files(dir).to_vec())
                .unwrap_or_default();
            let mut targets = PathTargets::default();
            for file in &files {
                let gated = self.gated_mods_of(file);
                targets.gated.extend(gated.path_targets.iter().cloned());
                targets.open.extend(gated.open_path_targets.iter().cloned());
            }
            self.path_targets = Some(targets);
        }
        self.path_targets.get_or_insert_with(PathTargets::default)
    }

    /// A declarer's test-gated `mod`s, kept while the declarer is unchanged.
    fn gated_mods_of(&mut self, declarer: &Path) -> std::rc::Rc<GatedMods> {
        if let Some(gated) = self.gated_mods.get(declarer) {
            return gated.clone();
        }
        let dir = declarer.parent().map(Path::to_path_buf).unwrap_or_default();
        // rustc resolves a `#[path]` inside an inline module relative to the
        // inline module's directory: for a non-mod-rs declarer (`routes.rs`)
        // that directory is named after the file (`routes/<inline>/`).
        let inline_dir = if mod_rs(declarer) {
            dir.clone()
        } else {
            declarer
                .file_stem()
                .map_or_else(|| dir.clone(), |stem| dir.join(stem))
        };
        let gated = file_cache::derived(declarer, |content| {
            let mut gated = GatedMods::default();
            if may_gate_tests(content)
                && let Ok(ast) = syn::parse_file(content)
            {
                gated_mod_paths(&ast.items, "", &dir, &inline_dir, false, &mut gated);
            }
            gated
        })
        .unwrap_or_default();
        self.gated_mods
            .insert(declarer.to_path_buf(), gated.clone());
        gated
    }
}

/// A declarer's test-gated `mod`s: the `::`-joined chain paths (relative to
/// the declarer: `b` for `#[cfg(test)] mod b;`, `a::b` inside an inline
/// `mod a { .. }`) of those loaded by their own name, and the files of those
/// given a `#[path]`.
#[derive(Clone, Default)]
struct GatedMods {
    chains: HashSet<String>,
    path_targets: Vec<PathBuf>,
    /// The ungated `mod`s loaded by their own name (`::`-joined as `chains`)
    /// and the files of the ungated ones given a `#[path]`.
    open_chains: HashSet<String>,
    open_path_targets: Vec<PathBuf>,
}

/// The `#[path = ".."]` value of `attrs`.
fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("path"))
        .find_map(|attr| attr.meta.require_name_value().ok())
        .and_then(|named| string_literal(&named.value))
}

/// The value of a string literal expression.
fn string_literal(expr: &syn::Expr) -> Option<String> {
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(value),
        ..
    }) = expr
    else {
        return None;
    };
    Some(value.value())
}

/// Collect the test-gated `mod`s among `items` (prefixed by `prefix`; `dir`
/// is the directory their out-of-line files are loaded from), recursing into
/// every inline module. A gated inline module gates everything it nests: its
/// own name goes in as a chain, and the `#[path]` modules nested in it are
/// gated too (they name files only test builds load). A gated `#[path]`
/// module names its file directly, relative to `dir` (rustc resolves it from
/// the declaring file's directory). `inline_dir` is where the inline modules
/// of `items` nest: `dir` itself for a mod-rs declarer, `dir/<file stem>` for
/// any other, so a `#[path]` inside `mod v2 { .. }` of `routes.rs` resolves
/// under `routes/v2/`. `within_gated` says `items` already sit in a gated
/// inline module.
fn gated_mod_paths(
    items: &[Item],
    prefix: &str,
    dir: &Path,
    inline_dir: &Path,
    within_gated: bool,
    out: &mut GatedMods,
) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        let name = syn::ext::IdentExt::unraw(&module.ident).to_string();
        let path = format!("{prefix}{name}");
        let gated = within_gated || attrs_test_only(&module.attrs);
        match &module.content {
            Some((_, content)) => {
                // Everything nested inside an inline module lives under it.
                let nested = inline_dir.join(&name);
                if gated {
                    out.chains.insert(path.clone());
                }
                gated_mod_paths(content, &format!("{path}::"), &nested, &nested, gated, out);
            }
            None if gated => match path_attr(&module.attrs) {
                Some(target) => out.path_targets.extend(lexical(&dir.join(target))),
                None => {
                    out.chains.insert(path);
                }
            },
            None => {
                // An ungated out-of-line module: every build loads its file.
                match path_attr(&module.attrs) {
                    Some(target) => out.open_path_targets.extend(lexical(&dir.join(target))),
                    None => {
                        out.open_chains.insert(path);
                    }
                }
            }
        }
    }
}

/// Whether `declarer` is a mod-rs file (`lib.rs`, `main.rs`, `mod.rs`): its
/// inline modules nest under its own directory, any other file's under a
/// directory named after the file.
fn mod_rs(declarer: &Path) -> bool {
    matches!(
        declarer.file_stem().and_then(|stem| stem.to_str()),
        Some("lib" | "main" | "mod")
    )
}

/// The directory whose files are the child modules of a `#[path]` target:
/// a `mod.rs` (or crate root) owns its directory, any other file owns the
/// directory named after it (`helpers.rs` -> `helpers/`).
fn child_dir(target: &Path) -> PathBuf {
    if mod_rs(target) {
        target.parent().unwrap_or(target).to_path_buf()
    } else {
        target.with_extension("")
    }
}

/// Whether the module chain `rest` is exactly the `::`-joined `path`: an
/// ungated declaration loads the file only when it names the file's own
/// module, not one of the directories above it.
fn chain_is(rest: &[String], path: &str) -> bool {
    let mut rest = rest.iter();
    path.split("::")
        .all(|part| rest.next().is_some_and(|segment| segment == part))
        && rest.next().is_none()
}

/// Whether the module chain `rest` begins with the `::`-joined `path`.
fn chain_starts_with(rest: &[String], path: &str) -> bool {
    let mut rest = rest.iter();
    path.split("::")
        .all(|part| rest.next().is_some_and(|segment| segment == part))
}

/// `path` made absolute (against the current directory) with `.` / `..`
/// folded lexically — symlinks are not resolved. `None` when there is no
/// current directory or a `..` climbs above the root.
fn lexical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut folded = PathBuf::new();
    // `components()` already drops every `.` of an absolute path.
    for component in absolute.components() {
        if component == Component::ParentDir {
            if !folded.pop() {
                return None;
            }
        } else {
            folded.push(component);
        }
    }
    Some(folded)
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

/// The HTTP methods a `#[route(..)]` argument can name.
const ROUTE_METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// A top-level fn of a route file, with what tells the `#[route]` storage
/// entry of one apart from a same-named fn behind another `cfg`.
#[derive(Clone)]
pub struct FnDefinition {
    name: String,
    /// The `#[route(<method>)]` method, `get` for a bare `#[route]`.
    method: String,
    /// `quote!(#sig)` of the signature: the text `StoredRouteInfo::fn_sig_str` keeps.
    pub(super) signature: String,
    test_only: bool,
}

/// The `#[route]` method of `attrs`, lower case (`get` when none is named).
/// Both spellings count: `#[route(..)]` and `#[vespera::route(..)]`.
fn route_method(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter(|attr| crate::route::check_route_by_meta(&attr.meta))
        .filter_map(|attr| attr.meta.require_list().ok())
        .filter_map(|list| {
            list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .ok()
        })
        .flatten()
        .find_map(|meta| {
            let syn::Meta::Path(path) = meta else {
                return None;
            };
            let method = path.get_ident()?.to_string().to_ascii_lowercase();
            ROUTE_METHODS.contains(&method.as_str()).then_some(method)
        })
        .unwrap_or_else(|| "get".to_owned())
}

/// The top-level fns of `file` (kept while the file is unchanged; empty when
/// it does not parse). Called for route files only, so it is not spared a
/// parse by a text filter.
pub fn fn_definitions(file: &Path) -> Vec<FnDefinition> {
    struct FnDefinitions(Vec<FnDefinition>);
    file_cache::derived(file, |content| {
        FnDefinitions(
            syn::parse_file(content)
                .map(|ast| {
                    ast.items
                        .iter()
                        .filter_map(|item| match item {
                            Item::Fn(function) => {
                                let sig = &function.sig;
                                Some(FnDefinition {
                                    name: sig.ident.to_string().trim_start_matches("r#").to_owned(),
                                    method: route_method(&function.attrs),
                                    signature: quote::quote!(#sig).to_string(),
                                    test_only: attrs_test_only(&function.attrs),
                                })
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    })
    .map(|fns| fns.0.clone())
    .unwrap_or_default()
}

/// Whether the `#[route]` storage entry `name` / `method` / `signature` of a
/// file whose fns are `defs` is a test-only fn. Same-named fns are told
/// apart by method and signature: `#[cfg(test)] #[route(get)] fn list()`
/// next to `#[route(post)] fn list()` leaves the production `POST` route
/// documented.
pub fn route_test_only(defs: &[FnDefinition], name: &str, method: &str, signature: &str) -> bool {
    let same_name: Vec<&FnDefinition> = defs.iter().filter(|def| def.name == name).collect();
    if same_name.is_empty() {
        return false;
    }
    if same_name.iter().all(|def| def.test_only) {
        return true;
    }
    let same_method: Vec<&FnDefinition> = same_name
        .into_iter()
        .filter(|def| def.method.eq_ignore_ascii_case(method))
        .collect();
    // The stored signature is alias-expanded and may differ from the source
    // text, so the signature only decides between several same-method twins.
    let matching: Vec<&FnDefinition> = if same_method.len() > 1 {
        same_method
            .into_iter()
            .filter(|def| def.signature == signature)
            .collect()
    } else {
        same_method
    };
    !matching.is_empty() && matching.iter().all(|def| def.test_only)
}

/// A `(line, column)` source position, as `proc_macro2` reports it.
type Position = (usize, usize);

/// The test-only items of one file: the `(name, position)` of every struct /
/// enum gated by a test-only `#[cfg(..)]` on itself or an enclosing scope,
/// and the position range of every other gated item or statement (an inline
/// module, a fn / impl / trait / const / static, an item or statement macro
/// such as `#[cfg(test)] schema_type!(..)`) — whatever it nests is
/// test-only too (a `#[derive(Schema)]` inside a `#[test] fn`, a
/// `schema_type!` inside `mod tests`). Ranges are compared by position, so
/// an item sharing a line with a gated one is not gated by it.
#[derive(Default)]
struct GatedItems {
    items: HashSet<(String, Position)>,
    ranges: Vec<(Position, Position)>,
}

impl GatedItems {
    fn contains(&self, ident: &str, position: Position) -> bool {
        self.items.contains(&(ident.to_owned(), position))
            || self
                .ranges
                .iter()
                .any(|&(start, end)| (start..=end).contains(&position))
    }
}

/// Where `span` starts.
fn start_of(span: proc_macro2::Span) -> Position {
    let start = span.start();
    (start.line, start.column)
}

/// Where `span` ends.
fn end_of(span: proc_macro2::Span) -> Position {
    let end = span.end();
    (end.line, end.column)
}

/// `file`'s [`GatedItems`] (empty for an unreadable file), kept while the
/// file is unchanged (`file_cache::derived`): `vespera!` asks for every
/// schema file on every invocation, route-cache hits included.
fn gated_items(file: &Path) -> std::rc::Rc<GatedItems> {
    file_cache::derived(file, |content| {
        let mut gated = GatedItems::default();
        // `contains("test")`, not `may_gate_tests`: a `#[test]` fn gates its
        // body without any `cfg`.
        if content.contains("test") {
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
        if !outer && (attrs_test_only(attrs) || attrs_test_harness(attrs)) {
            let span = node.span();
            self.out.ranges.push((start_of(span), end_of(span)));
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
                    .insert((ident.to_string(), start_of(ident.span())));
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

/// `(file, ident, (line, column))` of a `#[derive(Schema)]` source identity
/// (`<file>::<Ident>@<line>:<column>`, see `schema_impl`).
fn parse_identity(identity: &str) -> Option<(&str, &str, Position)> {
    let (path_ident, position) = identity.rsplit_once('@')?;
    let (file, ident) = path_ident.rsplit_once("::")?;
    let (line, column) = position.split_once(':')?;
    Some((file, ident, (line.parse().ok()?, column.parse().ok()?)))
}

impl TestScope {
    /// Whether the schema with `identity` is test-only (its file or the item
    /// itself).
    fn schema_test_only(
        &mut self,
        identity: &str,
        items: &mut HashMap<String, std::rc::Rc<GatedItems>>,
    ) -> bool {
        let Some((file, ident, position)) = parse_identity(identity) else {
            return false;
        };
        self.file_test_only(Path::new(file))
            || items
                .entry(file.to_owned())
                .or_insert_with(|| gated_items(Path::new(file)))
                .contains(ident, position)
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
            test_only.file_path == route.file_path
                && test_only.function_name == route.function_name
                && (test_only.method.is_empty()
                    || test_only.method.eq_ignore_ascii_case(&route.method))
        })
    });
    let mut test_structs = test_structs.into_iter();
    view.structs
        .retain(|_| !test_structs.next().unwrap_or_default());
    Cow::Owned(view)
}

#[cfg(test)]
mod tests;
