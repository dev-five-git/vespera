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
//! handler is. An alias that cannot be resolved is left untouched; the
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
    matches!(&sig.output, ReturnType::Type(_, ty) if alias_candidate(ty).is_some())
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

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` (the handler's file) and (if given) the crate source
/// directory `src_dir`.
///
/// Returns `true` when the return type changed.
pub fn expand_return_type_alias(
    sig: &mut syn::Signature,
    same_path: Option<&Path>,
    same_file: Option<&syn::File>,
    src_dir: Option<&Path>,
) -> bool {
    let handler = sig.ident.clone();
    let ReturnType::Type(_, ty) = &mut sig.output else {
        return false;
    };
    let mut scope = same_file.map(|file| {
        NextScope::Ready(Scope {
            module: fn_module(&file.items, &handler).unwrap_or_default(),
            file: Cow::Borrowed(file),
            path: same_path.map(source_key),
        })
    });
    let mut expanded = false;
    // The return type before the first expansion.
    let mut original: Option<String> = None;
    // Keyed by the resolved definition, not the ident: `type Result<T> =
    // crate::error::Result<Json<T>>;` names another module's `Result`.
    let mut seen: Vec<DefinitionKey> = Vec::new();
    while seen.len() < MAX_ALIAS_DEPTH {
        let Some(name) = alias_candidate(ty) else {
            break;
        };
        // An alias found under `src/` names its file, parsed only now that
        // the alias body turned out to be another alias candidate.
        let here = scope.take().and_then(NextScope::materialize);
        if matches!(&**ty, Type::Path(path) if is_external_path(&path.path, here.as_ref())) {
            break;
        }
        let qualifier = alias_qualifier(ty);
        let Some((alias, alias_scope)) =
            find_alias(&name, qualifier.as_ref(), here.as_ref(), src_dir)
        else {
            break;
        };
        let key = definition_key(&alias, &alias_scope);
        if seen.contains(&key) {
            break;
        }
        let Some(next) = apply_alias(&alias, ty) else {
            break;
        };
        if !expanded {
            original = Some(quote::quote!(#ty).to_string());
        }
        **ty = next;
        seen.push(key);
        expanded = true;
        // The alias body names types where the alias is defined.
        scope = Some(alias_scope);
    }
    // A self-recursive (`type Result<T> = Result<T>;`) or cyclic alias may
    // leave the type as it was.
    original.is_some_and(|original| quote::quote!(#ty).to_string() != original)
}

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
}

/// The [`Qualifier`] of `ty`. `None` for a bare name, `self::X`, and the
/// longer `crate` / `super` chains (`super::super::X`), which are resolved
/// like a bare name.
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
    let needle = format!("type {name}");
    let mut test_scope = TestScope::new(Some(src_dir));
    let mut definitions = Vec::new();
    for path in file_cache::list_rs_files(src_dir).iter() {
        if let Some(file) = parsed_with(path, &needle)
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
                // Never the handler module's own alias.
                Qualifier::Super => {
                    !scope.is_some_and(|scope| is_own_alias(scope, own.as_ref(), found, alias))
                }
            })
            .cloned()
            .collect();
        // `super::` never names the handler module's own alias: with only
        // that one left, the type is not expanded. A named module (or the
        // crate root) that declares a struct / enum of that name holds the
        // return type itself, so a same-named alias elsewhere is unrelated.
        if named.is_empty()
            && (matches!(qualifier, Qualifier::Super)
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
        .all(|(other, ..)| same_definition(&alias, &other))
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
        (None, _) => own.is_some_and(|own| same_definition(own, alias)),
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

/// The module each glob of this crate among `items` imports from (`crate::
/// models::*` → `models`, `super::*` → the parent, `crate::*` → the root).
/// A glob of another crate, or of `self`, names no module of this crate.
fn glob_parents(items: &[syn::Item]) -> Vec<Qualifier> {
    fn walk(
        tree: &syn::UseTree,
        root: Option<&syn::Ident>,
        parent: Option<&syn::Ident>,
        items: &[syn::Item],
        out: &mut Vec<Qualifier>,
    ) {
        match tree {
            syn::UseTree::Path(path) => walk(
                &path.tree,
                root.or(Some(&path.ident)),
                Some(&path.ident),
                items,
                out,
            ),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    walk(tree, root, parent, items, out);
                }
            }
            syn::UseTree::Glob(_) => {
                let local =
                    root.is_some_and(|root| is_local_root(root) || declares_module(items, root));
                if let Some(parent) = parent.filter(|_| local) {
                    out.extend(match parent.to_string().as_str() {
                        "crate" => Some(Qualifier::CrateRoot),
                        "super" => Some(Qualifier::Super),
                        "self" => None,
                        _ => Some(Qualifier::Module(
                            syn::ext::IdentExt::unraw(parent).to_string(),
                        )),
                    });
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
        }
    }
    let mut out = Vec::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            walk(&item_use.tree, None, None, items, &mut out);
        }
    }
    out
}

/// Where a plain `use` among `items` imports `name` from: the module
/// (`models` for `use crate::models::ApiResponse;`), the crate root (`use
/// crate::ApiResponse;`) or the parent module (`use super::ApiResponse;`).
/// `None` without such an import, for a rename / glob, and for `self`.
fn import_parent(items: &[syn::Item], name: &str) -> Option<Qualifier> {
    /// `Some(parent)` once the import of `name` is found (stops the walk).
    #[allow(clippy::option_option)]
    fn walk(
        tree: &syn::UseTree,
        parent: Option<&syn::Ident>,
        name: &str,
    ) -> Option<Option<Qualifier>> {
        match tree {
            syn::UseTree::Path(path) => walk(&path.tree, Some(&path.ident), name),
            syn::UseTree::Name(used) => (used.ident == name).then(|| {
                parent.and_then(|parent| match parent.to_string().as_str() {
                    "crate" => Some(Qualifier::CrateRoot),
                    "super" => Some(Qualifier::Super),
                    "self" => None,
                    _ => Some(Qualifier::Module(
                        syn::ext::IdentExt::unraw(parent).to_string(),
                    )),
                })
            }),
            syn::UseTree::Group(group) => {
                group.items.iter().find_map(|tree| walk(tree, parent, name))
            }
            syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => None,
        }
    }
    items
        .iter()
        .find_map(|item| match item {
            syn::Item::Use(item_use) => walk(&item_use.tree, None, name),
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

/// Whether the module a `use` imports `name` from ([`import_parent`])
/// declares a struct / enum / union `name`: a named module (see
/// [`nominal_in_module`]), the crate root (the top level of `lib.rs` /
/// `main.rs` under `src_dir`), or the parent module (the inline parent in
/// the handler's file; for a file module, any file's top level — the parent
/// file is not known).
fn imported_nominal(
    name: &str,
    parent: &Qualifier,
    scope: &Scope<'_>,
    src_dir: Option<&Path>,
) -> bool {
    let top_level = |root_only: bool| {
        src_dir.is_some_and(|src_dir| {
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                (!root_only || file_module(path, src_dir).is_none())
                    && parsed_with(path, name)
                        .is_some_and(|file| declares_nominal(&file.items, name))
            })
        })
    };
    match parent {
        Qualifier::Module(module) => nominal_in_module(name, module, scope, src_dir),
        Qualifier::CrateRoot => top_level(true),
        Qualifier::Super => match scope.module.split_last() {
            Some((_, parent)) => {
                let parent = Scope {
                    file: scope.file.clone(),
                    module: parent.to_vec(),
                    path: scope.path.clone(),
                };
                declares_nominal(parent.items(), name)
            }
            None => top_level(false),
        },
    }
}

/// Whether a module named `module` (an inline `mod` of the handler's file,
/// or a file / inline module under `src_dir`) declares a struct / enum /
/// union `name`.
fn nominal_in_module(name: &str, module: &str, scope: &Scope<'_>, src_dir: Option<&Path>) -> bool {
    fn walk(items: &[syn::Item], own: Option<&str>, name: &str, module: &str) -> bool {
        (own == Some(module) && declares_nominal(items, name))
            || items.iter().any(|item| match item {
                syn::Item::Mod(syn::ItemMod {
                    attrs,
                    ident,
                    content: Some((_, inner)),
                    ..
                }) if !attrs_test_only(attrs) => {
                    walk(inner, Some(&ident.to_string()), name, module)
                }
                _ => false,
            })
    }
    walk(&scope.file.items, None, name, module)
        || src_dir.is_some_and(|src_dir| {
            file_cache::list_rs_files(src_dir).iter().any(|path| {
                parsed_with(path, name).is_some_and(|file| {
                    walk(
                        &file.items,
                        file_module(path, src_dir).as_deref(),
                        name,
                        module,
                    )
                })
            })
        })
}

/// `crate` / `self` / `super`: a path root inside the current crate.
fn is_local_root(ident: &syn::Ident) -> bool {
    ident == "crate" || ident == "self" || ident == "super"
}

/// Whether the return type `ty` is a qualified path into another crate
/// (`axum::response::Result<..>`): never one of this crate's aliases. A
/// path whose first segment is `crate` / `self` / `super`, a module the
/// scope declares (`mod error;`) or a name it imports from this crate
/// (`use crate::error;`) stays a candidate; without the file, any other
/// qualified path counts as external.
fn is_external_path(path: &syn::Path, scope: Option<&Scope<'_>>) -> bool {
    let first = &path.segments[0].ident;
    path.segments.len() > 1
        && !is_local_root(first)
        && !scope.is_some_and(|scope| {
            let items = scope.items();
            declares_module(items, first)
                || import_kind(items, &first.to_string()) == Some(Import::Local)
        })
}

/// Whether `items` declare `mod <name>` (inline or file module).
fn declares_module(items: &[syn::Item], name: &syn::Ident) -> bool {
    items
        .iter()
        .any(|item| matches!(item, syn::Item::Mod(module) if module.ident == *name))
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
            let local_module = |ident: &syn::Ident| declares_module(items, ident);
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

/// The parsed file at `path`. Files whose text lacks `needle` are skipped
/// without parsing.
fn parsed_with(path: &Path, needle: &str) -> Option<syn::File> {
    file_cache::get_file_content(path)
        .filter(|content| content.contains(needle))
        .and_then(|_| parse_items(path))
}

/// The items of the file at `path`. Function bodies are skipped: only item
/// names, `use` trees and inline modules are ever read here.
fn parse_items(path: &Path) -> Option<syn::File> {
    file_cache::get_shallow_parsed_file(path)
}

fn same_definition(a: &syn::ItemType, b: &syn::ItemType) -> bool {
    let tokens = |alias: &syn::ItemType| {
        let generics = &alias.generics;
        let ty = &alias.ty;
        quote::quote!(#generics = #ty).to_string()
    };
    tokens(a) == tokens(b)
}

/// `alias` applied to the use site `ty` (`ApiResult<User>` →
/// `Result<Json<User>, AppError>`), or `None` when the arguments do not fit
/// the alias's type parameters.
fn apply_alias(alias: &syn::ItemType, ty: &Type) -> Option<Type> {
    let segment = crate::schema_macro::type_utils::last_path_segment(ty)?;
    let args: Vec<&Type> = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
        // `None` (and the `Fn(..)` sugar, which never reaches a return type).
        _ => Vec::new(),
    };
    let params: Vec<&syn::TypeParam> = alias.generics.type_params().collect();
    if args.len() > params.len() {
        return None;
    }
    let names: Vec<String> = params.iter().map(|param| param.ident.to_string()).collect();
    let mut concrete: Vec<Type> = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        let ty = if let Some(arg) = args.get(index) {
            (*arg).clone()
        } else {
            // A default may name earlier parameters (`type R<T, E =
            // Wrapper<T>>`): they are already concrete.
            let earlier: Vec<&Type> = concrete.iter().collect();
            crate::parser::schema::substitute_type(
                &param.default.as_ref()?.1,
                &names[..index],
                &earlier,
            )
        };
        concrete.push(ty);
    }
    let concrete: Vec<&Type> = concrete.iter().collect();
    Some(crate::parser::schema::substitute_type(
        &alias.ty, &names, &concrete,
    ))
}

#[cfg(test)]
mod tests;
