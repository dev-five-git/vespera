//! Where `export_app!` publishes a child app's OpenAPI document and how a
//! parent's `merge = [path::App]` finds it again.
//!
//! Sidecars are **crate-scoped** — `target/vespera/export/<crate>/<App>.openapi.json`
//! — so two crates exporting a same-named app never overwrite each other.
//! A parent resolves a merge path the way rustc would before picking a file:
//!
//! 1. `use` aliases in the `vespera!` call-site file are expanded
//!    (`use child::ChildApp as Admin;` → `merge = [Admin]` is `child::ChildApp`),
//!    so the export name — not the alias — selects the file;
//! 2. the first segment names the crate (`crate` / `self` / `super` / a local
//!    module → the current crate);
//! 3. a renamed dependency (`[dependencies] alias = { package = .. }`) falls
//!    back to the only crate that exported that app name.
//!
//! Every resolved file carries the key `<crate>/<App>`; the parent asserts at
//! compile time that the merged type's `__VESPERA_EXPORT_KEY` matches, so a
//! wrong guess is a compile error instead of a silently mismatched spec.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

/// `target/vespera/export` — every crate's exported specs live below it.
pub(super) fn export_root(vespera_dir: &Path) -> PathBuf {
    vespera_dir.join("export")
}

/// The current crate's directory name under [`export_root`]: its crate name
/// as other crates spell it in paths (`CARGO_CRATE_NAME`, falling back to the
/// package name with `-` → `_`).
pub(super) fn current_export_crate() -> String {
    crate::env_access::var("CARGO_CRATE_NAME")
        .or_else(|_| crate::env_access::var("CARGO_PKG_NAME"))
        .map_or_else(|_| "default".to_string(), |name| name.replace('-', "_"))
}

/// Stable path of `app`'s exported spec published by crate `krate`.
pub(super) fn export_spec_path(vespera_dir: &Path, krate: &str, app: &str) -> PathBuf {
    export_root(vespera_dir)
        .join(krate)
        .join(format!("{app}.openapi.json"))
}

/// [`export_spec_path`] with its directory created (test fixtures).
#[cfg(test)]
pub(super) fn child_spec_path(vespera_dir: &Path, krate: &str, app: &str) -> PathBuf {
    let path = export_spec_path(vespera_dir, krate, app);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

/// Compile-time identity of one exported app (`<crate>/<App>` hashed).
pub(super) fn export_key(krate: &str, app: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "vespera:export-key:v1".hash(&mut hasher);
    krate.hash(&mut hasher);
    app.hash(&mut hasher);
    hasher.finish()
}

/// A merge path resolved to the export that should back it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedExport {
    /// The exporting crate's directory name.
    pub(super) krate: String,
    /// The `export_app!` name (alias resolved).
    pub(super) app: String,
    /// The spec sidecar (may not exist — the caller reports that).
    pub(super) spec_file: PathBuf,
}

/// The `use` aliases of one source file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct UseAliases {
    /// Local name → full path segments, for every name the file binds to one
    /// path.
    pub(super) paths: HashMap<String, Vec<String>>,
    /// Local names bound to different paths by different `use` items (in
    /// different scopes of the file). The call site's scope is not known, so
    /// neither path is chosen: a merge path starting with one is an error.
    pub(super) ambiguous: HashSet<String>,
}

/// `use` aliases of one source file (see [`UseAliases`]).
pub(super) fn use_aliases(file: Option<&Path>) -> UseAliases {
    let mut aliases = UseAliases::default();
    for (name, full) in use_bindings(file) {
        if aliases.ambiguous.contains(&name) {
            continue;
        }
        match aliases.paths.get(&name) {
            Some(bound) if *bound != full => {
                aliases.paths.remove(&name);
                aliases.ambiguous.insert(name);
            }
            Some(_) => {}
            None => {
                aliases.paths.insert(name, full);
            }
        }
    }
    aliases
}

/// Every `use` binding of `file` in source order: `(local name, full path)`.
fn use_bindings(file: Option<&Path>) -> Vec<(String, Vec<String>)> {
    struct Uses<'a>(&'a mut Vec<(String, Vec<String>)>);
    impl<'ast> syn::visit::Visit<'ast> for Uses<'_> {
        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            collect_use(&item_use.tree, &mut Vec::new(), self.0);
        }
    }
    let mut bindings = Vec::new();
    let Some(ast) = file.and_then(crate::schema_macro::file_cache::get_parsed_file) else {
        return bindings;
    };
    // Every `use` of the file, including one in a function body (the
    // `vespera!` call often sits in `fn create_app()`) or an inline module.
    syn::visit::visit_file(&mut Uses(&mut bindings), &ast);
    bindings
}

fn collect_use(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    bindings: &mut Vec<(String, Vec<String>)>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use(&path.tree, prefix, bindings);
            prefix.pop();
        }
        // `use child::{self, X}` binds `child` itself, not a name `self`.
        syn::UseTree::Name(name) if name.ident == "self" => {
            if let Some(last) = prefix.last() {
                bindings.push((last.clone(), prefix.clone()));
            }
        }
        syn::UseTree::Name(name) => {
            let mut full = prefix.clone();
            full.push(name.ident.to_string());
            bindings.push((name.ident.to_string(), full));
        }
        syn::UseTree::Rename(rename) if rename.ident == "self" => {
            bindings.push((rename.rename.to_string(), prefix.clone()));
        }
        syn::UseTree::Rename(rename) => {
            let mut full = prefix.clone();
            full.push(rename.ident.to_string());
            bindings.push((rename.rename.to_string(), full));
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_use(tree, prefix, bindings);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// The compile error for a merge path whose first segment is an ambiguous
/// `use` name of the call-site file.
pub(super) fn ambiguous_alias_error(
    merge_path: &syn::Path,
    aliases: &UseAliases,
) -> Option<String> {
    let first = merge_path.segments.first()?.ident.to_string();
    aliases.ambiguous.contains(&first).then(|| {
        format!(
            "vespera! merge: `{first}` is bound by `use` items of this file to different paths \
             (in different scopes), so the merge path cannot tell which app it names — spell \
             the merge path out in full (`krate::Module::App`) instead of `{first}`"
        )
    })
}

/// Resolve `merge_path` against the call-site `aliases` and the exported
/// specs under `vespera_dir`.
pub(super) fn resolve_export(
    merge_path: &syn::Path,
    aliases: &UseAliases,
    vespera_dir: &Path,
) -> Option<ResolvedExport> {
    let mut segments: Vec<String> = merge_path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    // An alias only ever replaces the first segment (that is the name the
    // file's `use` brought into scope). An ambiguous one resolves to nothing:
    // `ambiguous_alias_error` reports it.
    if let Some(first) = segments.first() {
        if aliases.ambiguous.contains(first) {
            return None;
        }
        if let Some(full) = aliases.paths.get(first) {
            segments.splice(..1, full.iter().cloned());
        }
    }
    let app = segments.last()?.clone();
    let current = current_export_crate();
    let named = segments
        .first()
        .filter(|_| segments.len() > 1)
        .filter(|first| !matches!(first.as_str(), "crate" | "self" | "super"))
        .cloned();
    let candidates = named.into_iter().chain(std::iter::once(current.clone()));
    for krate in candidates {
        let spec_file = export_spec_path(vespera_dir, &krate, &app);
        if spec_file.is_file() {
            return Some(ResolvedExport {
                krate,
                app,
                spec_file,
            });
        }
    }
    // Renamed dependency: the only crate that exported this app name.
    let mut exporters = exporters_of(vespera_dir, &app);
    let krate = if exporters.len() == 1 {
        exporters.pop()?
    } else {
        current
    };
    Some(ResolvedExport {
        spec_file: export_spec_path(vespera_dir, &krate, &app),
        krate,
        app,
    })
}

/// Every crate directory under [`export_root`] holding `app`'s spec, sorted.
fn exporters_of(vespera_dir: &Path, app: &str) -> Vec<String> {
    let mut exporters: Vec<String> = std::fs::read_dir(export_root(vespera_dir))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join(format!("{app}.openapi.json")).is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    exporters.sort();
    exporters
}

/// The compile error for a merge path that only the renamed-dependency
/// fallback could resolve while several crates exported that app name — e.g.
/// a stale `export/<old crate>/` directory left by a renamed crate.  Guessing
/// one would merge an arbitrary document (the key assertion then fails with a
/// misleading message), so the candidates are listed instead.
pub(super) fn ambiguous_export_error(
    merge_path: &syn::Path,
    aliases: &UseAliases,
    vespera_dir: &Path,
) -> Option<String> {
    let resolved = resolve_export(merge_path, aliases, vespera_dir)?;
    if resolved.spec_file.is_file() {
        return None;
    }
    let exporters = exporters_of(vespera_dir, &resolved.app);
    (exporters.len() > 1).then(|| {
        format!(
            "vespera! merge: `{}` does not name the crate that exports `{app}`, and several \
             crates did: {} (under '{}') — spell the merge path as `<crate>::...::{app}`, or \
             delete the stale directories",
            super::openapi_io::display_merge_path(merge_path),
            exporters
                .iter()
                .map(|krate| format!("`{krate}`"))
                .collect::<Vec<_>>()
                .join(", "),
            export_root(vespera_dir).display(),
            app = resolved.app,
        )
    })
}

/// One compile-time assertion per merged app whose spec was found: the
/// merged type must be the export that published it, so an alias or crate
/// guess that picked another app's spec fails the build instead of merging
/// the wrong document.
pub(super) fn merged_export_checks(
    merge: &[syn::Path],
    merge_specs: &super::cache::MergeSpecCache,
) -> Vec<proc_macro2::TokenStream> {
    merge
        .iter()
        .filter_map(|path| {
            let resolved = merge_specs.resolve(path)?;
            if !resolved.spec_file.is_file() {
                return None;
            }
            let key = export_key(&resolved.krate, &resolved.app);
            let message = format!(
                "vespera! merge: `{}` is not `export_app!({})` of crate `{}`, whose OpenAPI \
                 document was found for it — spell the merge path as `<crate>::...::{}`",
                super::openapi_io::display_merge_path(path),
                resolved.app,
                resolved.krate,
                resolved.app
            );
            Some(quote::quote_spanned! { syn::spanned::Spanned::span(path)=>
                const _: () = ::core::assert!(#path::__VESPERA_EXPORT_KEY == #key, #message);
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn publish(vespera_dir: &Path, krate: &str, app: &str) -> PathBuf {
        let path = export_spec_path(vespera_dir, krate, app);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        path
    }

    fn resolve(path: &str, uses: &str, vespera_dir: &Path) -> Option<(String, String)> {
        let file = vespera_dir.join("call_site.rs");
        fs::write(&file, uses).unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let aliases = use_aliases(Some(&file));
        resolve_export(&syn::parse_str(path).unwrap(), &aliases, vespera_dir)
            .map(|resolved| (resolved.krate, resolved.app))
    }

    fn pair(krate: &str, app: &str) -> (String, String) {
        (krate.to_string(), app.to_string())
    }

    #[test]
    fn merge_paths_resolve_to_crate_scoped_exports() {
        let temp = TempDir::new().unwrap();
        let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "parent");
        let dir = temp.path();
        publish(dir, "child", "ChildApp");
        publish(dir, "other", "ChildApp");
        publish(dir, "renamed_pkg", "OnlyApp");
        publish(dir, "parent", "LocalApp");

        // The first segment picks among same-named exports.
        assert_eq!(
            resolve("child::ChildApp", "", dir),
            Some(pair("child", "ChildApp"))
        );
        assert_eq!(
            resolve("other::ChildApp", "", dir),
            Some(pair("other", "ChildApp"))
        );
        // `use .. as Alias` resolves to the export name and its crate.
        assert_eq!(
            resolve("Admin", "use other::ChildApp as Admin;", dir),
            Some(pair("other", "ChildApp"))
        );
        assert_eq!(
            resolve(
                "kids::ChildApp",
                "use ::child as kids; use std::{fmt, io::*};",
                dir
            ),
            Some(pair("child", "ChildApp"))
        );
        assert_eq!(
            resolve("ChildApp", "use child::{ChildApp}; fn helper() {}", dir),
            Some(pair("child", "ChildApp"))
        );
        // Local modules and `crate::` paths fall back to the current crate.
        assert_eq!(
            resolve("crate::api::LocalApp", "", dir),
            Some(pair("parent", "LocalApp"))
        );
        assert_eq!(
            resolve("api::LocalApp", "", dir),
            Some(pair("parent", "LocalApp"))
        );
        // A renamed dependency: the unique exporter of that app name.
        assert_eq!(
            resolve("dep::OnlyApp", "", dir),
            Some(pair("renamed_pkg", "OnlyApp"))
        );
        // Ambiguous / missing: the current crate's (absent) file is reported.
        assert_eq!(
            resolve("dep::ChildApp", "", dir),
            Some(pair("parent", "ChildApp"))
        );
        // ... and the ambiguity itself is a compile error naming the
        // candidates; a found or unique export is not.
        let ambiguous = |path: &str| {
            ambiguous_export_error(&syn::parse_str(path).unwrap(), &UseAliases::default(), dir)
        };
        insta::assert_snapshot!(
            "ambiguous_export",
            ambiguous("dep::ChildApp")
                .unwrap()
                .replace(&export_root(dir).display().to_string(), "<EXPORT>")
        );
        assert_eq!(ambiguous("child::ChildApp"), None);
        assert_eq!(ambiguous("dep::OnlyApp"), None);
        assert_eq!(ambiguous("dep::Missing"), None);
        assert_eq!(
            ambiguous_export_error(
                &syn::parse_str("dep::ChildApp").unwrap(),
                &UseAliases::default(),
                &dir.join("absent")
            ),
            None
        );
        assert_eq!(
            resolve("dep::Missing", "", dir),
            Some(pair("parent", "Missing"))
        );
    }

    /// Found exports get a key assertion; missing ones (reported elsewhere)
    /// and an unresolvable environment get none.
    #[test]
    fn merged_exports_are_checked_at_compile_time() {
        let temp = TempDir::new().unwrap();
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
        let vespera_dir =
            crate::vespera_impl::path_utils::find_target_dir(temp.path()).join("vespera");
        publish(&vespera_dir, "child", "ChildApp");
        let call_site = temp.path().join("lib.rs");
        fs::write(&call_site, "use child::ChildApp as Admin;").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let merge: Vec<syn::Path> =
            vec![syn::parse_quote!(Admin), syn::parse_quote!(child::Missing)];

        let cache = super::super::cache::MergeSpecCache::with_call_site(Some(&call_site));
        let checks = merged_export_checks(&merge, &cache);
        assert_eq!(checks.len(), 1);
        let item: syn::Item = syn::parse2(checks[0].clone()).unwrap();
        insta::assert_snapshot!(
            "merged_export_check",
            prettyplease::unparse(&syn::parse_quote!(#item)).replace(
                &format!("{}u64", export_key("child", "ChildApp")),
                "<KEY>u64"
            )
        );

        let _manifest = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        let cache = super::super::cache::MergeSpecCache::with_call_site(None);
        assert!(merged_export_checks(&merge, &cache).is_empty());
    }

    /// `vespera!` rejects an ambiguous merge path before anything is merged;
    /// without a locatable target directory there is nothing to check.
    #[test]
    fn ambiguous_merge_paths_are_rejected_by_the_cache() {
        let temp = TempDir::new().unwrap();
        let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "parent");
        let merge: Vec<syn::Path> = vec![syn::parse_quote!(dep::ChildApp)];
        {
            let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
            let vespera_dir =
                crate::vespera_impl::path_utils::find_target_dir(temp.path()).join("vespera");
            let cache = super::super::cache::MergeSpecCache::with_call_site(None);
            cache.check_unambiguous(&merge).unwrap();
            publish(&vespera_dir, "child", "ChildApp");
            publish(&vespera_dir, "stale_child", "ChildApp");
            let error = cache.check_unambiguous(&merge).unwrap_err().to_string();
            assert!(
                error.contains("`child`, `stale_child`"),
                "unexpected error: {error}"
            );
        }
        let _manifest = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        let cache = super::super::cache::MergeSpecCache::with_call_site(None);
        cache.check_unambiguous(&merge).unwrap();
    }

    /// `use child::{self, X}` (and `self as c`) alias the module itself.
    #[test]
    fn self_imports_alias_the_module() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("lib.rs");
        std::fs::write(
            &file,
            "use child::{self, ChildApp}; use other::{self as o}; use {self};",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let aliases = use_aliases(Some(&file));
        assert_eq!(aliases.paths.get("child"), Some(&vec!["child".to_string()]));
        assert_eq!(aliases.paths.get("o"), Some(&vec!["other".to_string()]));
        assert_eq!(
            aliases.paths.get("ChildApp"),
            Some(&vec!["child".to_string(), "ChildApp".to_string()])
        );
        assert!(!aliases.paths.contains_key("self"));
    }

    /// A `use` inside a function body (where `vespera!` is usually called) or
    /// an inline module is an alias of the call site too.
    #[test]
    fn aliases_inside_a_function_body_and_inline_modules_count() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("app.rs");
        std::fs::write(
            &file,
            "pub fn create_app() { use child::ChildApp as Admin; }\nmod inner { use other::Other as O; }\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let aliases = use_aliases(Some(&file));
        assert_eq!(
            aliases.paths.get("Admin"),
            Some(&vec!["child".to_string(), "ChildApp".to_string()])
        );
        assert_eq!(
            aliases.paths.get("O"),
            Some(&vec!["other".to_string(), "Other".to_string()])
        );
    }

    /// The same local name bound to two paths in two scopes of one file is
    /// neither alias: the call site's scope is unknown.
    #[test]
    fn a_name_bound_to_two_paths_in_two_scopes_is_ambiguous() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("app.rs");
        std::fs::write(
            &file,
            "use admin_crate::AdminApp as App;\n\
             pub mod public { use public_crate::PublicApp as App; }\n\
             pub mod third { use admin_crate::AdminApp as App; }\n\
             use other::Other as Same;\n\
             pub mod again { use other::Other as Same; }\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let aliases = use_aliases(Some(&file));
        assert!(aliases.ambiguous.contains("App"));
        assert!(!aliases.paths.contains_key("App"));
        // Equal bindings are one alias.
        assert_eq!(
            aliases.paths.get("Same"),
            Some(&vec!["other".to_string(), "Other".to_string()])
        );
        let merge: syn::Path = syn::parse_str("App").unwrap();
        assert!(ambiguous_alias_error(&merge, &aliases).is_some());
        let plain: syn::Path = syn::parse_str("Same").unwrap();
        assert!(ambiguous_alias_error(&plain, &aliases).is_none());
        assert!(resolve_export(&merge, &aliases, temp.path()).is_none());
    }

    /// An alias bound to two paths is an error at the merge path that uses it,
    /// not a guess (the cache reports it before anything is merged).
    #[test]
    fn ambiguous_alias_merge_path_is_rejected_by_the_cache() {
        let temp = TempDir::new().unwrap();
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
        let call_site = temp.path().join("call_site.rs");
        fs::write(
            &call_site,
            "use admin::AdminApp as App;\npub mod public { use public::PublicApp as App; }\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let cache = super::super::cache::MergeSpecCache::with_call_site(Some(&call_site));
        let merge: Vec<syn::Path> = vec![syn::parse_quote!(App)];
        let error = cache.check_unambiguous(&merge).unwrap_err().to_string();
        assert!(error.contains("bound by `use` items"), "{error}");
    }

    #[test]
    fn aliases_without_a_readable_call_site_are_empty() {
        assert_eq!(use_aliases(None), UseAliases::default());
        assert_eq!(
            use_aliases(Some(Path::new("missing/call_site.rs"))),
            UseAliases::default()
        );
    }

    #[test]
    fn export_crate_and_key() {
        {
            let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "my_app");
            assert_eq!(current_export_crate(), "my_app");
        }
        let _crate = crate::env_access::test_env::remove_var("CARGO_CRATE_NAME");
        {
            let _pkg = crate::env_access::test_env::set_var("CARGO_PKG_NAME", "my-app");
            assert_eq!(current_export_crate(), "my_app");
        }
        let _pkg = crate::env_access::test_env::remove_var("CARGO_PKG_NAME");
        assert_eq!(current_export_crate(), "default");
        assert_ne!(export_key("a", "App"), export_key("b", "App"));
        assert_eq!(export_key("a", "App"), export_key("a", "App"));
    }
}
