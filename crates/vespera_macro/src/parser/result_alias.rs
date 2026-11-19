//! Expansion of type aliases used as a handler's return type.
//!
//! `type ApiResult<T> = Result<Json<T>, AppError>;` is a common way to spell
//! handler results, but the response parser only understands the shapes it
//! can see (`Result<..>`, `Json<..>`, ...). Before an operation is built the
//! return type is therefore expanded through the alias definitions vespera can
//! find:
//!
//! 1. a `type` item in the handler's own module (unless that module imports
//!    the name from another crate: `use axum::response::Result;`, or declares
//!    a struct / enum / union of that name, imports one from the module
//!    that declares it, or renames another item of this crate to it: `use
//!    crate::models::Envelope as ApiResponse;`: then the name is that item,
//!    not an alias), then
//! 2. a single, unambiguous `type` item anywhere in the handler's file or
//!    under the crate's `src/` (two different definitions with the same name
//!    are left unexpanded).
//!
//! A qualified name skips step 1 and prefers the definitions of the module
//! it names: `v2::ApiResult` those `v2` declares, `crate::ApiResult` the
//! crate root's (`lib.rs` / `main.rs`), `super::ApiResult` the parent's (an
//! inline parent module in the handler's file, else any definition but the
//! handler module's own). Only the innermost module name of a longer path is
//! matched: `crate::admin::users::ApiResult` names `users` (no full module
//! path is resolved, so two `users` modules are not told apart). A plain `use
//! crate::api::v2::ApiResult;` in the handler's module counts as that
//! qualifier (`v2`). A path-less `use anyhow;` imports another crate.
//!
//! Test-only code (`#[cfg(test)]` modules, items and files) never defines a
//! handler's alias. Generic parameters (including defaults such as `type R<T,
//! E = AppError>`) are substituted, and an alias of an alias is followed a few
//! levels deep — resolved where the outer alias is defined, not where the
//! handler is: the alias body's names are resolved in the module that defines
//! the alias, and the use-site arguments substituted into it in the handler's
//! own scope. An alias that cannot be resolved is left untouched; the
//! response parser then documents it by description only instead of
//! inventing a schema.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use syn::{ReturnType, Type};

use crate::{
    collector::test_scope::{TestScope, attrs_test_only},
    file_utils::normalize_path_key,
    parser::schema::is_builtin_openapi_type,
    schema_macro::file_cache,
};

/// How many alias-of-alias levels are followed before giving up.
const MAX_ALIAS_DEPTH: usize = 8;

/// Idents that are never user aliases (axum response wrappers handled by the
/// response parser). `Result` is special-cased: only a one-argument `Result<T>`
/// may be a user alias (`type Result<T> = std::result::Result<T, AppError>`).
const NON_ALIAS_IDENTS: [&str; 11] = [
    "Json",
    "Html",
    "Sse",
    "Bytes",
    "BytesMut",
    "Body",
    "Response",
    "Redirect",
    "ErrorResponse",
    "HeaderMap",
    "CookieJar",
];

/// Whether `sig`'s return type could name a user alias (so looking one up is
/// worth reading the source file).
pub fn needs_alias_expansion(sig: &syn::Signature) -> bool {
    matches!(&sig.output, ReturnType::Type(_, ty) if has_alias_position(ty))
}

/// Whether an alias can apply to `ty` or to a response position nested in it
/// (a tuple element or a `Result` argument, as [`expand_position`] walks them).
fn has_alias_position(ty: &Type) -> bool {
    if alias_candidate(ty).is_some() {
        return true;
    }
    match ty {
        Type::Tuple(tuple) => tuple.elems.iter().any(has_alias_position),
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Result" && result_has_alias_arg(segment)),
        _ => false,
    }
}

/// Whether a `Result` path segment has a type argument with an alias position.
fn result_has_alias_arg(segment: &syn::PathSegment) -> bool {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return false;
    };
    args.args
        .iter()
        .any(|arg| matches!(arg, syn::GenericArgument::Type(inner) if has_alias_position(inner)))
}

/// Where a name is resolved: a parsed file and the inline module path of the
/// module inside it.
#[derive(Clone)]
struct Scope<'a> {
    file: Cow<'a, syn::File>,
    module: Vec<String>,
    /// The [`source_key`] of the file `file` is, when it is known (the
    /// handler's file, or a file under `src/`); `None` when the caller gave
    /// no path.
    path: Option<String>,
}

/// The identity of a source file: its path normalized against the current
/// directory, so the handler's path and the walked `src/` paths compare equal
/// however each was spelled.
fn source_key(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    normalize_path_key(&path.to_string_lossy(), &cwd)
}

impl Scope<'_> {
    /// The module's own items (the file's top level when the path is gone).
    fn items(&self) -> &[syn::Item] {
        module_items(&self.file.items, &self.module).unwrap_or(&self.file.items)
    }
}

/// The items of the inline module at `path` below `items`.
fn module_items<'a>(items: &'a [syn::Item], path: &[String]) -> Option<&'a [syn::Item]> {
    let Some((first, rest)) = path.split_first() else {
        return Some(items);
    };
    items.iter().find_map(|item| match item {
        syn::Item::Mod(syn::ItemMod {
            ident,
            content: Some((_, inner)),
            ..
        }) if ident == first => module_items(inner, rest),
        _ => None,
    })
}

/// The inline module path of the function `name` among `items`, preferring
/// the shallowest one: a same-named helper in an inline module never shadows
/// a top-level handler, and test-only (`#[cfg(test)]`) modules are skipped.
fn fn_module(items: &[syn::Item], name: &syn::Ident) -> Option<Vec<String>> {
    if items
        .iter()
        .any(|item| matches!(item, syn::Item::Fn(function) if function.sig.ident == *name))
    {
        return Some(Vec::new());
    }
    items.iter().find_map(|item| match item {
        syn::Item::Mod(syn::ItemMod {
            attrs,
            ident,
            content: Some((_, inner)),
            ..
        }) if !attrs_test_only(attrs) => fn_module(inner, name).map(|mut path| {
            path.insert(0, ident.to_string());
            path
        }),
        _ => None,
    })
}

mod expansion;
mod imports;
pub use expansion::expand_return_type_alias;
use imports::{declares_mod, glob_declares_module, imported_nominal};

/// Identifies one alias definition: its tokens, its file (`None` for the
/// handler's own, already parsed file) and the inline module path inside it.
type DefinitionKey = (String, Option<PathBuf>, Vec<String>);

/// The [`DefinitionKey`] of `alias`, defined in `scope`.
fn definition_key(alias: &syn::ItemType, scope: &NextScope<'_>) -> DefinitionKey {
    let tokens = quote::quote!(#alias).to_string();
    match scope {
        NextScope::Ready(scope) => (tokens, None, scope.module.clone()),
        NextScope::File(path, module) => (tokens, Some(PathBuf::clone(path)), module.clone()),
    }
}

/// The ident of `ty` if it may name a user alias.
fn alias_candidate(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() || type_path.path.leading_colon.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    let ident = segment.ident.to_string();
    if ident == "Result" {
        // `Result<T, E>` is std's; only a single-argument `Result<T>` can be a
        // user alias (axum's own `Result<T>` simply finds no alias).
        let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
            return None;
        };
        let type_args = args
            .args
            .iter()
            .filter(|arg| matches!(arg, syn::GenericArgument::Type(_)))
            .count();
        return (type_args == 1).then_some(ident);
    }
    if is_builtin_openapi_type(&ident) || NON_ALIAS_IDENTS.contains(&ident.as_str()) {
        return None;
    }
    Some(ident)
}

/// Where a qualified return type names its alias.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Qualifier {
    /// `v2::ApiResult<..>`, `crate::api::v2::ApiResult<..>`: the module
    /// `v2` declares it.
    Module(String),
    /// `crate::ApiResult<..>`: the crate root (`lib.rs` / `main.rs`).
    CrateRoot,
    /// `super::ApiResult<..>`: the handler module's parent.
    Super,
    /// A `crate` / `super` chain longer than one segment (`super::super::X`):
    /// no ancestor beyond the parent is looked up, so the alias is left
    /// unexpanded (a bare-name lookup would find the handler module's own).
    Unresolved,
}

/// The [`Qualifier`] of `ty`. `None` for a bare name and `self::X`.
fn alias_qualifier(ty: &Type) -> Option<Qualifier> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segments = &type_path.path.segments;
    let module = &segments.iter().rev().nth(1)?.ident;
    if !is_local_root(module) {
        return Some(Qualifier::Module(
            syn::ext::IdentExt::unraw(module).to_string(),
        ));
    }
    match (segments.len(), module.to_string().as_str()) {
        (2, "crate") => Some(Qualifier::CrateRoot),
        (2, "super") => Some(Qualifier::Super),
        (_, "crate" | "super") => Some(Qualifier::Unresolved),
        _ => None,
    }
}

/// One `type <name>` definition: the alias, the scope it is defined in, the
/// innermost module that declares it (inline `mod`, else the file's module;
/// `None` when unknown), and whether that is the crate root (the top level
/// of `lib.rs` / `main.rs` under `src/`).
type Definition<'a> = (syn::ItemType, NextScope<'a>, Option<String>, bool);

/// The scope an alias was found in: parsed (the handler's file), or a file
/// under `src/` with the inline module path inside it, parsed on demand.
#[derive(Clone)]
enum NextScope<'a> {
    Ready(Scope<'a>),
    File(Rc<PathBuf>, Vec<String>),
}

impl<'a> NextScope<'a> {
    fn materialize(self) -> Option<Scope<'a>> {
        match self {
            Self::Ready(scope) => Some(scope),
            Self::File(path, module) => parse_items(&path).map(|file| Scope {
                file: Cow::Owned(file),
                module,
                path: Some(source_key(&path)),
            }),
        }
    }
}

/// One `type <name>` definition under `src/`, as plain data: the alias
/// tokens, its file, the inline module path inside it, the innermost
/// declaring module and whether that is the crate root.
#[derive(Clone)]
struct SrcDefinition {
    alias: String,
    file: Rc<PathBuf>,
    module: Vec<String>,
    innermost: Option<String>,
    root: bool,
}

/// Every non-test `type <name>` definition under `src_dir`.
///
/// Memoised across invocations while every file and listing the scan read
/// is unchanged (`file_cache::Tracked`): each `#[route]` returning an alias
/// otherwise re-listed `src/` and re-parsed every file declaring the name.
fn src_definitions(src_dir: &Path, name: &str) -> Rc<[SrcDefinition]> {
    type Memo = HashMap<(PathBuf, String), file_cache::Tracked<Rc<[SrcDefinition]>>>;
    thread_local! {
        static MEMO: RefCell<Memo> = RefCell::new(Memo::new());
    }
    let key = (src_dir.to_path_buf(), name.to_owned());
    let hit = MEMO.with(|memo| {
        memo.borrow_mut()
            .get_mut(&key)
            .and_then(file_cache::Tracked::get)
    });
    if let Some(hit) = hit {
        return hit;
    }
    let entry = file_cache::Tracked::compute(|| scan_src_definitions(src_dir, name));
    let found = Rc::clone(entry.peek());
    MEMO.with(|memo| memo.borrow_mut().insert(key, entry));
    found
}

#[cfg(test)]
thread_local! {
    /// How many times [`scan_src_definitions`] ran (memo misses).
    static SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn scan_src_definitions(src_dir: &Path, name: &str) -> Rc<[SrcDefinition]> {
    #[cfg(test)]
    SCANS.with(|scans| scans.set(scans.get() + 1));
    let mut test_scope = TestScope::new(Some(src_dir));
    let mut definitions = Vec::new();
    for path in file_cache::list_rs_files(src_dir).iter() {
        if let Some(file) = parsed_declaring_type(path, name)
            && !test_scope.file_test_only(path)
        {
            let module = file_module(path, src_dir);
            let root = module.is_none();
            let mut found = Vec::new();
            collect_aliases(&Cow::Owned(file), module.as_deref(), root, name, &mut found);
            let file = Rc::new(path.clone());
            definitions.extend(found.into_iter().map(|(alias, scope, innermost, root)| {
                SrcDefinition {
                    alias: quote::quote!(#alias).to_string(),
                    file: Rc::clone(&file),
                    module: scope.module,
                    innermost,
                    root,
                }
            }));
        }
    }
    definitions.into()
}

/// The `type <name>` definition visible from `scope`, if any, with the
/// scope it is defined in. A `qualifier` skips the handler's own module:
/// `v2::ApiResult` prefers the definitions module `v2` declares,
/// `crate::ApiResult` those of the crate root, `super::ApiResult` the parent
/// module's (an inline parent in the same file, else any definition but
/// the handler module's own).
fn find_alias<'a>(
    name: &str,
    qualifier: Option<&Qualifier>,
    scope: Option<&Scope<'a>>,
    src_dir: Option<&Path>,
) -> Option<(syn::ItemType, NextScope<'a>)> {
    if qualifier == Some(&Qualifier::Super)
        && let Some(found) = scope.and_then(|scope| inline_parent_alias(scope, name))
    {
        return Some(found);
    }
    // `use crate::api::v2::ApiResult;` names the module it imports from,
    // exactly like the qualified `v2::ApiResult`.
    let mut imported = None;
    if let Some(scope) = scope.filter(|_| qualifier.is_none()) {
        let items = scope.items();
        if let Some(alias) = items.iter().find_map(|item| alias_item(item, name)) {
            return Some((alias, NextScope::Ready(scope.clone())));
        }
        // A struct / enum of that name in the handler's module (or imported
        // from the module that declares one) is the return type itself.
        if declares_nominal(items, name) {
            return None;
        }
        match import_kind(items, name) {
            // `use axum::response::Result;` — the name is another crate's
            // item, so a same-named alias elsewhere in this crate is
            // unrelated.
            Some(Import::External) => return None,
            Some(Import::Local)
                if import_parent(items, name)
                    .is_some_and(|parent| imported_nominal(name, &parent, scope, src_dir)) =>
            {
                return None;
            }
            // `use crate::models::Envelope as ApiResponse;` — the name is
            // another item of this crate, so a same-named alias elsewhere is
            // unrelated.
            Some(Import::Local) if renames_to(items, name) => return None,
            Some(Import::Local) => imported = import_parent(items, name),
            // `use crate::models::*;` / `use super::*;` brings a same-named
            // struct / enum into scope just as a named import does.
            None if glob_parents(items)
                .iter()
                .any(|parent| imported_nominal(name, parent, scope, src_dir)) =>
            {
                return None;
            }
            _ => {}
        }
        if imported == Some(Qualifier::Super)
            && let Some(found) = inline_parent_alias(scope, name)
        {
            return Some(found);
        }
    }
    let qualifier = qualifier.or(imported.as_ref());
    let mut definitions: Vec<Definition<'a>> = Vec::new();
    if let Some(scope) = scope {
        let mut found = Vec::new();
        collect_aliases(&scope.file, None, false, name, &mut found);
        definitions.extend(found.into_iter().map(|(alias, found, innermost, root)| {
            let found = Scope {
                path: scope.path.clone(),
                ..found
            };
            (alias, NextScope::Ready(found), innermost, root)
        }));
    }
    if let Some(src_dir) = src_dir {
        definitions.extend(
            src_definitions(src_dir, name)
                .iter()
                .filter_map(|definition| {
                    Some((
                        syn::parse_str(&definition.alias).ok()?,
                        NextScope::File(Rc::clone(&definition.file), definition.module.clone()),
                        definition.innermost.clone(),
                        definition.root,
                    ))
                }),
        );
    }
    if let Some(qualifier) = qualifier {
        let own =
            scope.and_then(|scope| scope.items().iter().find_map(|item| alias_item(item, name)));
        let named: Vec<Definition<'a>> = definitions
            .iter()
            .filter(|(alias, found, module, root)| match qualifier {
                Qualifier::Module(qualifier) => module.as_deref() == Some(qualifier.as_str()),
                Qualifier::CrateRoot => *root,
                Qualifier::Unresolved => false,
                // Never the handler module's own alias.
                Qualifier::Super => {
                    !scope.is_some_and(|scope| is_own_alias(scope, own.as_ref(), found, alias))
                }
            })
            .cloned()
            .collect();
        // `super::` never names the handler module's own alias: with only
        // that one left, the type is not expanded. An unresolved chain names
        // nothing. A named module (or the crate root) that declares a struct /
        // enum of that name holds the return type itself, so a same-named
        // alias elsewhere is unrelated.
        if named.is_empty()
            && (matches!(qualifier, Qualifier::Super | Qualifier::Unresolved)
                || scope.is_some_and(|scope| imported_nominal(name, qualifier, scope, src_dir)))
        {
            return None;
        }
        // Not declared in that module itself (a `pub use` re-export): any
        // unique definition.
        if !named.is_empty() {
            definitions = named;
        }
    }
    let mut definitions = definitions.into_iter();
    let (alias, alias_scope, ..) = definitions.next()?;
    // The same alias seen twice (the handler's file is under `src/` too, a
    // re-declaration) is fine; two different definitions are ambiguous
    // without name resolution.
    definitions
        .all(|(other, other_scope, ..)| same_definition(&alias, &alias_scope, &other, &other_scope))
        .then_some((alias, alias_scope))
}

/// Whether the definition `found` (of `alias`) is the alias of `scope`'s own
/// module: the same module of the same file. The file is compared by identity
/// (see [`source_key`]); when the scope's file is unknown, by its tokens
/// (`own`, the alias the scope's module declares).
fn is_own_alias(
    scope: &Scope<'_>,
    own: Option<&syn::ItemType>,
    found: &NextScope<'_>,
    alias: &syn::ItemType,
) -> bool {
    match (&scope.path, found) {
        (Some(_), NextScope::Ready(found)) => found.module == scope.module,
        (Some(own_path), NextScope::File(path, module)) => {
            *module == scope.module && source_key(path) == *own_path
        }
        (None, _) => own.is_some_and(|own| same_tokens(own, alias)),
    }
}

/// The `type <name>` alias of the inline parent of `scope`'s module, with
/// the parent's scope.
fn inline_parent_alias<'a>(
    scope: &Scope<'a>,
    name: &str,
) -> Option<(syn::ItemType, NextScope<'a>)> {
    let (_, parent) = scope.module.split_last()?;
    let parent_scope = Scope {
        file: scope.file.clone(),
        module: parent.to_vec(),
        path: scope.path.clone(),
    };
    let alias = parent_scope
        .items()
        .iter()
        .find_map(|item| alias_item(item, name))?;
    Some((alias, NextScope::Ready(parent_scope)))
}

/// The module a source file under `src_dir` declares: its stem, the directory
/// name for a `mod.rs`, nothing for the crate root (`lib.rs` / `main.rs`
/// directly in `src_dir`). A `main.rs` / `lib.rs` deeper down (`src/admin/
/// main.rs`, `src/bin/tool/main.rs`) is an ordinary module named by its stem.
fn file_module(file: &Path, src_dir: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    let parent = file.parent()?;
    let at_root = parent
        .strip_prefix(src_dir)
        .is_ok_and(|rest| rest.as_os_str().is_empty());
    match stem {
        "lib" | "main" if at_root => None,
        "mod" => parent.file_name()?.to_str().map(str::to_owned),
        _ => Some(stem.to_owned()),
    }
}

/// `item` when it is a non-test `type <name>` alias.
fn alias_item(item: &syn::Item, name: &str) -> Option<syn::ItemType> {
    match item {
        syn::Item::Type(alias) if alias.ident == name && !attrs_test_only(&alias.attrs) => {
            Some(alias.clone())
        }
        _ => None,
    }
}

/// Every non-test `type <name>` item of `file` (inline modules searched,
/// test-only modules skipped), each with the module it is defined in;
/// `file_module` is the module the file itself declares, `file_root` whether
/// the file is the crate root.
// The `Cow` itself is cloned into each scope (cheap when borrowed).
#[allow(clippy::ptr_arg)]
fn collect_aliases<'a>(
    file: &Cow<'a, syn::File>,
    file_module: Option<&str>,
    file_root: bool,
    name: &str,
    out: &mut Vec<(syn::ItemType, Scope<'a>, Option<String>, bool)>,
) {
    fn walk(
        items: &[syn::Item],
        name: &str,
        path: &mut Vec<String>,
        found: &mut Vec<(syn::ItemType, Vec<String>)>,
    ) {
        for item in items {
            if let Some(alias) = alias_item(item, name) {
                found.push((alias, path.clone()));
            } else if let syn::Item::Mod(syn::ItemMod {
                attrs,
                ident,
                content: Some((_, inner)),
                ..
            }) = item
                && !attrs_test_only(attrs)
            {
                path.push(ident.to_string());
                walk(inner, name, path, found);
                path.pop();
            }
        }
    }
    let mut found = Vec::new();
    walk(&file.items, name, &mut Vec::new(), &mut found);
    out.extend(found.into_iter().map(|(alias, module)| {
        let innermost = module
            .last()
            .cloned()
            .or_else(|| file_module.map(str::to_owned));
        let root = file_root && module.is_empty();
        (
            alias,
            Scope {
                file: file.clone(),
                module,
                // The caller knows the file's path (see `find_alias`).
                path: None,
            },
            innermost,
            root,
        )
    }));
}

/// Whether `items` declare a non-test struct / enum / union `name`.
fn declares_nominal(items: &[syn::Item], name: &str) -> bool {
    items.iter().any(|item| match item {
        syn::Item::Struct(item) => item.ident == name && !attrs_test_only(&item.attrs),
        syn::Item::Enum(item) => item.ident == name && !attrs_test_only(&item.attrs),
        syn::Item::Union(item) => item.ident == name && !attrs_test_only(&item.attrs),
        _ => false,
    })
}

/// The [`Qualifier`] of an import whose immediate parent segment is `parent`.
/// `nested` means that segment is itself preceded by another path segment, so
/// a `crate` / `super` there is one step of a chain (`super::super::X`): no
/// alias lookup resolves that, and it is left unexpanded.
fn import_qualifier(parent: &syn::Ident, nested: bool) -> Option<Qualifier> {
    match parent.to_string().as_str() {
        "crate" | "super" if nested => Some(Qualifier::Unresolved),
        "crate" => Some(Qualifier::CrateRoot),
        "super" => Some(Qualifier::Super),
        "self" => None,
        _ => Some(Qualifier::Module(
            syn::ext::IdentExt::unraw(parent).to_string(),
        )),
    }
}

/// The module each glob of this crate among `items` imports from (`crate::
/// models::*` → `models`, `super::*` → the parent, `crate::*` → the root).
/// A glob of another crate, or of `self`, names no module of this crate.
fn glob_parents(items: &[syn::Item]) -> Vec<Qualifier> {
    fn walk(
        tree: &syn::UseTree,
        root: Option<&syn::Ident>,
        parent: Option<&syn::Ident>,
        nested: bool,
        items: &[syn::Item],
        out: &mut Vec<Qualifier>,
    ) {
        match tree {
            syn::UseTree::Path(path) => walk(
                &path.tree,
                root.or(Some(&path.ident)),
                Some(&path.ident),
                parent.is_some(),
                items,
                out,
            ),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    walk(tree, root, parent, nested, items, out);
                }
            }
            syn::UseTree::Glob(_) => {
                let local = root.is_some_and(|root| {
                    is_local_root(root) || declares_mod(items, &root.to_string())
                });
                if let Some(parent) = parent.filter(|_| local) {
                    out.extend(import_qualifier(parent, nested));
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
        }
    }
    let mut out = Vec::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            walk(&item_use.tree, None, None, false, items, &mut out);
        }
    }
    out
}

/// Where a plain `use` among `items` imports `name` from: the module
/// (`models` for `use crate::models::ApiResponse;`), the crate root (`use
/// crate::ApiResponse;`) or the parent module (`use super::ApiResponse;`).
/// `None` without such an import, for a rename to another name / glob, and for
/// `self`.
fn import_parent(items: &[syn::Item], name: &str) -> Option<Qualifier> {
    /// `Some(parent)` once the import of `name` is found (stops the walk).
    #[allow(clippy::option_option)]
    fn walk(
        tree: &syn::UseTree,
        parent: Option<&syn::Ident>,
        nested: bool,
        name: &str,
    ) -> Option<Option<Qualifier>> {
        match tree {
            syn::UseTree::Path(path) => walk(&path.tree, Some(&path.ident), parent.is_some(), name),
            syn::UseTree::Name(used) => (used.ident == name)
                .then(|| parent.and_then(|parent| import_qualifier(parent, nested))),
            syn::UseTree::Group(group) => group
                .items
                .iter()
                .find_map(|tree| walk(tree, parent, nested, name)),
            // `use a::X as X;` brings `X` itself into scope, so it is followed
            // like `use a::X;`; a real rename (`X as Y`) imports another name.
            syn::UseTree::Rename(rename) => (rename.ident == name && rename.rename == name)
                .then(|| parent.and_then(|parent| import_qualifier(parent, nested))),
            syn::UseTree::Glob(_) => None,
        }
    }
    items
        .iter()
        .find_map(|item| match item {
            syn::Item::Use(item_use) => walk(&item_use.tree, None, false, name),
            _ => None,
        })
        .flatten()
}

/// Whether a `use` among `items` brings another item into scope under
/// `name` (`use crate::models::Envelope as ApiResponse;`); `X as X` is
/// not a rename.
fn renames_to(items: &[syn::Item], name: &str) -> bool {
    fn walk(tree: &syn::UseTree, name: &str) -> bool {
        match tree {
            syn::UseTree::Path(path) => walk(&path.tree, name),
            syn::UseTree::Rename(rename) => rename.rename == name && rename.ident != name,
            syn::UseTree::Group(group) => group.items.iter().any(|tree| walk(tree, name)),
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => false,
        }
    }
    items.iter().any(|item| match item {
        syn::Item::Use(item_use) => walk(&item_use.tree, name),
        _ => false,
    })
}

/// `crate` / `self` / `super`: a path root inside the current crate.
fn is_local_root(ident: &syn::Ident) -> bool {
    ident == "crate" || ident == "self" || ident == "super"
}

/// Whether the return type `ty` is a qualified path into another crate
/// (`axum::response::Result<..>`): never one of this crate's aliases. A
/// path whose first segment is `crate` / `self` / `super`, a module the
/// scope declares (`mod error;`), a name it imports from this crate
/// (`use crate::error;`), or a module a local glob (`use crate::api::*;`)
/// may bring in (`v2::ApiResult` with `api::v2`) stays a candidate; without
/// the file, any other qualified path counts as external. The glob test is
/// a name the scope does not import at all, and a local glob only counts when
/// one of its modules really declares `mod <first>` ([`glob_declares_module`]):
/// an un-imported `axum::` under `use crate::models::*;` stays external. An
/// external `use` of the qualifier still wins.
fn is_external_path(path: &syn::Path, scope: Option<&Scope<'_>>, src_dir: Option<&Path>) -> bool {
    let first = &path.segments[0].ident;
    let first_name = first.to_string();
    path.segments.len() > 1
        && !is_local_root(first)
        && !scope.is_some_and(|scope| {
            let items = scope.items();
            let named = import_kind(items, &first_name);
            declares_mod(items, &first_name)
                || named == Some(Import::Local)
                || (named.is_none() && glob_declares_module(&first_name, scope, src_dir))
        })
}

/// Where a `use` brings a name from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Import {
    /// `use crate::..` / `self::..` / `super::..`, or a module `items`
    /// declare (`use error::Result;` next to `mod error;`).
    Local,
    /// Any other root (`use axum::response::Result;`, `use ::std::..`).
    External,
}

/// The first `use` among the module's own `items` that brings `name` into
/// scope (`use a::Result;`, `use a::{B as Result};`). A `use` inside another
/// (inline) module does not reach this one. Without one, a glob of another
/// crate (`use axum::response::*;`) is the only way the name can be in
/// scope, unless a glob of this crate (`use crate::error::*;`) may bring it.
fn import_kind(items: &[syn::Item], name: &str) -> Option<Import> {
    let mut imports = Vec::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            let root = item_use.leading_colon.is_some().then_some(None);
            let local_module = |ident: &syn::Ident| declares_mod(items, &ident.to_string());
            use_tree_imports(&item_use.tree, root, &local_module, &mut imports);
        }
    }
    let named = imports
        .iter()
        .find(|(used, _)| used.as_deref() == Some(name));
    named.map(|(_, kind)| *kind).or_else(|| {
        let globs = || imports.iter().filter(|(used, _)| used.is_none());
        (globs().any(|(_, kind)| *kind == Import::External)
            && !globs().any(|(_, kind)| *kind == Import::Local))
        .then_some(Import::External)
    })
}

/// Every name `tree` brings into scope (`None` for a glob) with where it
/// comes from, in source order. `root` is `None` before the first path
/// segment, `Some(None)` after a leading `::` (always another crate),
/// `Some(Some(first))` otherwise. `local_module` tells whether a first
/// segment names a module of this crate.
#[allow(clippy::option_option)]
fn use_tree_imports(
    tree: &syn::UseTree,
    root: Option<Option<&syn::Ident>>,
    local_module: &dyn Fn(&syn::Ident) -> bool,
    out: &mut Vec<(Option<String>, Import)>,
) {
    // A path-less `use anyhow;` is its own first segment: an extern crate
    // (a local item is in scope without a `use`) unless it is a module of
    // this crate.
    let kind = |own: Option<&syn::Ident>| match root.unwrap_or(own) {
        Some(first) if is_local_root(first) || local_module(first) => Import::Local,
        _ => Import::External,
    };
    match tree {
        syn::UseTree::Path(path) => use_tree_imports(
            &path.tree,
            root.or(Some(Some(&path.ident))),
            local_module,
            out,
        ),
        syn::UseTree::Name(used) => {
            out.push((Some(used.ident.to_string()), kind(Some(&used.ident))));
        }
        syn::UseTree::Rename(rename) => {
            out.push((Some(rename.rename.to_string()), kind(Some(&rename.ident))));
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                use_tree_imports(tree, root, local_module, out);
            }
        }
        syn::UseTree::Glob(_) => out.push((None, kind(None))),
    }
}

/// The parsed file at `path`, when it may declare a `type` named `name` (see
/// [`declares_type_named`]). Files that do not are skipped without parsing.
fn parsed_declaring_type(path: &Path, name: &str) -> Option<syn::File> {
    file_cache::get_file_content(path)
        .filter(|content| declares_type_named(content, name))
        .and_then(|_| parse_items(path))
}

/// Whether `content` may declare `type <name>`: the name is preceded by the
/// keyword `type` and any whitespace (rustfmt never writes more than one
/// space, but `pub type\nName<T>` and `type  Name` are valid Rust). A false
/// positive only costs a parse, so the check errs on the side of matching.
fn declares_type_named(content: &str, name: &str) -> bool {
    content.match_indices(name).any(|(at, _)| {
        content[..at]
            .trim_end()
            .strip_suffix("type")
            .is_some_and(|before| !before.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
    })
}

/// The items of the file at `path`. Function bodies are skipped: only item
/// names, `use` trees and inline modules are ever read here.
fn parse_items(path: &Path) -> Option<syn::File> {
    file_cache::get_shallow_parsed_file(path)
}

/// Whether two definitions of an alias name are the same one: the same
/// tokens, defined in the same module. The same alias seen twice (the
/// handler's file is under `src/` too) qualifies; the same tokens in two
/// modules do not, since the names in the body resolve per module. A scope
/// whose file is unknown matches by module alone.
fn same_definition(
    alias: &syn::ItemType,
    alias_scope: &NextScope<'_>,
    other: &syn::ItemType,
    other_scope: &NextScope<'_>,
) -> bool {
    let (path, module) = defining_module(alias_scope);
    let (other_path, other_module) = defining_module(other_scope);
    same_tokens(alias, other)
        && module == other_module
        && (path.is_none() || other_path.is_none() || path == other_path)
}

/// The file (by [`source_key`], `None` when unknown) and inline module path a
/// definition's scope names.
fn defining_module<'s>(scope: &'s NextScope<'_>) -> (Option<String>, &'s [String]) {
    match scope {
        NextScope::Ready(scope) => (scope.path.clone(), &scope.module),
        NextScope::File(path, module) => (Some(source_key(path)), module),
    }
}

/// Whether two aliases have the same generics and body tokens.
fn same_tokens(a: &syn::ItemType, b: &syn::ItemType) -> bool {
    let tokens = |alias: &syn::ItemType| {
        let generics = &alias.generics;
        let ty = &alias.ty;
        quote::quote!(#generics = #ty).to_string()
    };
    tokens(a) == tokens(b)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_gate;

#[cfg(test)]
mod tests_glob;

#[cfg(test)]
mod tests_reexport;

#[cfg(test)]
mod tests_test_scope;
