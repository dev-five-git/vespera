//! Which route files and handlers are actually part of the compiled crate.
//!
//! The collector walks the route folder on disk, but not every file there is
//! compiled: `#[cfg(feature = "admin")] mod admin;` removes `admin.rs` (and
//! everything below it) when the feature is off. `#[route]` records every
//! handler it expands into `ROUTE_STORAGE`, so a gated module that IS
//! compiled normally reaches the collector through the fast path. The slow
//! path (a file `#[route]` never registered) cannot tell a compiled-out
//! module from one that was merely expanded after `vespera!`, so handlers
//! whose module declaration chain — or the handler itself — carries
//! `#[cfg(..)]` are recorded with those predicates: the router mounts them
//! under the same `#[cfg(..)]` (emitting `routes::admin::handler` for a
//! module the compiler removed would be an "unresolved path" error) and the
//! OpenAPI document leaves them out.
//!
//! Handlers inside an inline `mod name { .. }` block are rejected with a
//! clear error: the file-based router only addresses top-level functions of
//! a route file (`routes::file::handler`), so such a handler could never be
//! mounted.

use std::path::Path;

use syn::Item;

use crate::{route::extract_route_info, schema_macro::file_cache};

/// Whether `attrs` contain a `#[cfg(..)]`.
pub(super) fn is_cfg_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("cfg"))
}

/// The predicates of the `#[cfg(..)]` attributes in `attrs`, as token text.
pub(super) fn cfg_predicates(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .filter_map(|attr| attr.meta.require_list().ok())
        .map(|list| list.tokens.to_string())
        .collect()
}

/// The `#[cfg(..)]` predicates on the `mod` declarations that make the route
/// file with module `segments` (relative to `folder_path`) part of the crate.
///
/// `segments == ["admin", "users"]` reads `mod admin;` (declared in
/// `<folder>/mod.rs` or `<folder>.rs`) and `mod users;` (declared in
/// `<folder>/admin/mod.rs` or `<folder>/admin.rs`).
///
/// Each declarer's `mod` list is read once per file version (`file_cache::
/// derived`), not once per route file below it.
pub(super) fn module_cfg_predicates(folder_path: &Path, segments: &[String]) -> Vec<String> {
    let mut parent = folder_path.to_path_buf();
    let mut predicates = Vec::new();
    for segment in segments {
        for declarer in [parent.join("mod.rs"), sibling_rs_file(&parent)] {
            for module in declared_mods(&declarer)
                .iter()
                .filter(|module| &module.segment == segment)
            {
                predicates.extend(module.cfgs.iter().cloned());
            }
        }
        parent.push(segment);
    }
    predicates
}

/// The Rust module path of a route file, as its `mod` declarations name the
/// modules: a `#[path]` module is named by its declaration (`mod admin` with
/// `#[path = "admin_impl.rs"]` is `routes::admin`, though the file is
/// `admin_impl`), so the call path the router emits resolves. A segment no
/// declarer names keeps the file's own name.
pub(super) fn declared_module_segments(folder_path: &Path, segments: &[String]) -> Vec<String> {
    let mut parent = folder_path.to_path_buf();
    let mut names = Vec::with_capacity(segments.len());
    for segment in segments {
        let declared = [parent.join("mod.rs"), sibling_rs_file(&parent)]
            .iter()
            .find_map(|declarer| {
                declared_mods(declarer)
                    .into_iter()
                    .find(|module| &module.segment == segment)
            });
        names.push(declared.map_or_else(|| segment.clone(), |module| module.name));
        parent.push(segment);
    }
    names
}

/// One `mod` a declarer makes: the route-file segment it loads, its Rust
/// name and its `#[cfg(..)]` predicates.
#[derive(Clone)]
struct DeclaredMod {
    segment: String,
    name: String,
    cfgs: Vec<String>,
}

/// A declarer's top-level `mod`s (none when the file does not parse).
#[derive(Clone, Default)]
struct Declared(Vec<DeclaredMod>);

/// The top-level `mod`s of `declarer`, read once per file version
/// (`file_cache::derived`).
fn declared_mods(declarer: &Path) -> Vec<DeclaredMod> {
    file_cache::derived(declarer, |content| {
        Declared(
            syn::parse_file(content)
                .map(|file| {
                    file.items
                        .iter()
                        .filter_map(|item| match item {
                            Item::Mod(module) => Some(DeclaredMod {
                                segment: declared_segment(module),
                                name: crate::parser::strip_raw_prefix_owned(
                                    module.ident.to_string(),
                                ),
                                cfgs: cfg_predicates(&module.attrs),
                            }),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    })
    .map(|declared| declared.0.clone())
    .unwrap_or_default()
}

/// The route-file segment a `mod` declaration loads. A `#[path = ".."]`
/// module loads the file its path names (`mod admin` with `#[path =
/// "admin_impl.rs"]` loads `admin_impl`; a `mod.rs` path is named after its
/// directory), otherwise the module's own (unraw) name. `pub mod r#type;`
/// declares the `type.rs` segment.
fn declared_segment(module: &syn::ItemMod) -> String {
    let own = || crate::parser::strip_raw_prefix_owned(module.ident.to_string());
    let path = module
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("path"))
        .and_then(|attr| attr.meta.require_name_value().ok())
        .and_then(|named| match &named.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) => Some(value.value()),
            _ => None,
        });
    let Some(path) = path else {
        return own();
    };
    let path = Path::new(&path);
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod") => path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map_or_else(own, str::to_owned),
        Some(stem) => stem.to_owned(),
        None => own(),
    }
}

/// `<dir>.rs`: the file declaring the module `dir`, appended (not
/// `with_extension`, which would replace the `.v2` of `api.v2`).
pub(super) fn sibling_rs_file(dir: &Path) -> std::path::PathBuf {
    let mut name = dir.as_os_str().to_os_string();
    name.push(".rs");
    std::path::PathBuf::from(name)
}

/// `(module, handler)` of the first `#[route]` handler declared inside an
/// inline `mod { .. }` block of `items` (nested blocks included). Gated
/// blocks (`#[cfg(test)] mod tests { .. }`) are skipped unless `include`
/// says the handler was compiled (it is in `ROUTE_STORAGE`).
pub(super) fn inline_mod_route(
    items: &[Item],
    include: &dyn Fn(&str) -> bool,
) -> Option<(String, String)> {
    inline_mod_route_where(items, false, include, &|_| true)
}

/// [`inline_mod_route`] restricted to handlers `accept` admits — every hit is
/// considered, so a rejected first hit never hides a later one.  `gated`:
/// `items` sit inside a `#[cfg]`-gated module already (a nested
/// `mod tests { mod fixtures { .. } }` is as gated as `tests`).
fn inline_mod_route_where(
    items: &[Item],
    gated: bool,
    include: &dyn Fn(&str) -> bool,
    accept: &dyn Fn(&str) -> bool,
) -> Option<(String, String)> {
    items.iter().find_map(|item| {
        let Item::Mod(module) = item else {
            return None;
        };
        let (_, content) = module.content.as_ref()?;
        let gated = gated || is_cfg_gated(&module.attrs);
        content
            .iter()
            .find_map(|inner| match inner {
                Item::Fn(handler) if extract_route_info(&handler.attrs).is_some() => {
                    let name = handler.sig.ident.to_string();
                    ((!gated || include(&name)) && accept(&name))
                        .then(|| (module.ident.to_string(), name))
                }
                _ => None,
            })
            .or_else(|| inline_mod_route_where(content, gated, include, accept))
    })
}

/// The compile error for a handler found by [`inline_mod_route`].
pub(super) fn inline_mod_route_error(file: &Path, (module, handler): &(String, String)) -> String {
    format!(
        "vespera! macro: `#[route]` handler `{handler}` is declared inside the inline module `mod {module} {{ .. }}` in '{}'. Route handlers must be top-level functions of a route file — move `{handler}` out of `mod {module}` to the top level of this file, which keeps its route path. (Moving it into its own file `{module}.rs` instead would change the route to `/{module}`.)",
        file.display()
    )
}

/// `(module, handler)` of a fast-path `file`'s stored handler declared inside
/// an inline module.  `#[route]` recorded where each handler sits, so a known
/// inline one is rejected without re-parsing; only handlers whose scope could
/// not be located fall back to the source scan.  Inside rust-analyzer every
/// handler takes the source scan: the storage still holds the entry of a
/// handler since moved out of its inline module (a different scope, so it
/// was never replaced), which would keep the error alive.  The module
/// reported is the innermost one (`inner` of a stored `v2::inner`), as the
/// source scan reports it.
pub(super) fn stored_routes_inline_module(
    file: &Path,
    stored_routes: &[&crate::route_impl::StoredRouteInfo],
) -> Option<(String, String)> {
    let in_ide = crate::macro_storage::in_long_lived_server();
    if !in_ide
        && let Some(found) = stored_routes.iter().find_map(|stored| {
            stored
                .inline_module
                .as_ref()
                .filter(|module| !module.is_empty())
                .map(|module| {
                    let innermost = module.rsplit("::").next().unwrap_or(module);
                    (innermost.to_string(), stored.fn_name.clone())
                })
        })
    {
        return Some(found);
    }
    let stored_names: Vec<&str> = stored_routes
        .iter()
        .filter(|stored| in_ide || stored.inline_module.is_none())
        .map(|stored| stored.fn_name.as_str())
        .collect();
    if stored_names.is_empty() {
        return None;
    }
    // The stored entries that record an inline module: such a handler is
    // compiled inside one, whatever top-level handler shares its name.
    let inline_stored: Vec<&str> = stored_routes
        .iter()
        .filter(|stored| {
            stored
                .inline_module
                .as_deref()
                .is_some_and(|m| !m.is_empty())
        })
        .map(|stored| stored.fn_name.as_str())
        .collect();
    stored_inline_mod_route(file, &stored_names, &inline_stored)
}

/// Whether the fast-path `file` declares one of its stored handlers inside an
/// inline module. Only files whose text contains an inline `mod` are parsed.
///
/// A name is an inline handler when a stored entry records an inline module
/// for it (`inline_stored`); otherwise a top-level handler of the same name
/// means the stored entry is that one, and the inline twin is not stored.
pub(super) fn stored_inline_mod_route(
    file: &Path,
    stored_names: &[&str],
    inline_stored: &[&str],
) -> Option<(String, String)> {
    let content = file_cache::get_file_content(file)?;
    if !content.contains("mod ") || !content.contains("#[") {
        return None;
    }
    // [`inline_mod_route_where`] with `include` = `accept` = stored: a gated
    // block's handler counts exactly when it is stored, like any other.
    let routes = file_routes(file)?;
    routes
        .inline
        .iter()
        .find(|(_, name)| {
            stored_names.contains(&name.as_str())
                && (inline_stored.contains(&name.as_str()) || !routes.top_level.contains(name))
        })
        .cloned()
}

/// The `#[route]` handlers of one file, read off one parse and kept while
/// the file is unchanged (`file_cache::derived`): every `vespera!` asks for
/// each fast-path file.
struct FileRoutes {
    /// Top-level handler names.
    top_level: std::collections::HashSet<String>,
    /// `(module, handler)` of every handler inside an inline module, in
    /// [`inline_mod_route_where`]'s search order.
    inline: Vec<(String, String)>,
}

/// The cached [`FileRoutes`]: `None` when the file does not parse.
struct CachedRoutes(Option<std::rc::Rc<FileRoutes>>);

fn file_routes(file: &Path) -> Option<std::rc::Rc<FileRoutes>> {
    fn inline(items: &[Item], out: &mut Vec<(String, String)>) {
        let inline_mods = items.iter().filter_map(|item| match item {
            Item::Mod(syn::ItemMod {
                ident,
                content: Some((_, content)),
                ..
            }) => Some((ident, content)),
            _ => None,
        });
        for (ident, content) in inline_mods {
            for inner in content {
                if let Item::Fn(handler) = inner
                    && extract_route_info(&handler.attrs).is_some()
                {
                    out.push((ident.to_string(), handler.sig.ident.to_string()));
                }
            }
            inline(content, out);
        }
    }
    let cached = file_cache::derived(file, |content| {
        CachedRoutes(syn::parse_file(content).ok().map(|ast| {
            let mut routes = FileRoutes {
                top_level: ast
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::Fn(handler) if extract_route_info(&handler.attrs).is_some() => {
                            Some(handler.sig.ident.to_string())
                        }
                        _ => None,
                    })
                    .collect(),
                inline: Vec::new(),
            };
            inline(&ast.items, &mut routes.inline);
            std::rc::Rc::new(routes)
        }))
    })?;
    cached.0.clone()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain_module("mod admin;", &["admin"], &[])]
    #[case::gated_module("#[cfg(feature = \"admin\")] mod admin;", &["admin"], &["feature = \"admin\""])]
    #[case::two_cfgs("#[cfg(unix)] #[cfg(test)] #[cfg] mod admin;", &["admin"], &["unix", "test"])]
    #[case::other_module_gated("#[cfg(test)] mod tests; mod admin;", &["admin"], &[])]
    #[case::folder_root(r#"#[cfg(feature = "x")] mod admin;"#, &[], &[])]
    #[case::raw_keyword_module("#[cfg(unix)] pub mod r#type;", &["type"], &["unix"])]
    // A `#[path]` module loads the file its path names, not its own name.
    #[case::path_attribute(
        "#[cfg(feature = \"admin\")] #[path = \"admin_impl.rs\"] mod admin;",
        &["admin_impl"],
        &["feature = \"admin\""]
    )]
    #[case::path_attribute_by_own_name(
        "#[cfg(feature = \"admin\")] #[path = \"admin_impl.rs\"] mod admin;",
        &["admin"],
        &[]
    )]
    #[case::path_mod_rs("#[cfg(unix)] #[path = \"admin/mod.rs\"] mod admin;", &["admin"], &["unix"])]
    #[case::path_mod_rs_bare("#[cfg(unix)] #[path = \"mod.rs\"] mod admin;", &["admin"], &["unix"])]
    #[case::path_without_stem("#[cfg(unix)] #[path = \"\"] mod admin;", &["admin"], &["unix"])]
    #[case::non_literal_path("#[cfg(unix)] #[path = concat!(\"a\", \".rs\")] mod admin;", &["admin"], &["unix"])]
    fn module_declaration_cfg(
        #[case] mod_rs: &str,
        #[case] segments: &[&str],
        #[case] expected: &[&str],
    ) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.rs"), mod_rs).unwrap();
        file_cache::bump_epoch();
        let segments: Vec<String> = segments.iter().map(ToString::to_string).collect();
        assert_eq!(module_cfg_predicates(dir.path(), &segments), expected);
    }

    /// The call path names a `#[path]` module by its declaration; a file no
    /// declaration names keeps its own name.
    #[rstest]
    #[case::path_module(&["admin_impl"], &["admin"])]
    #[case::own_name(&["users"], &["users"])]
    #[case::undeclared(&["stray"], &["stray"])]
    fn declared_module_names(#[case] segments: &[&str], #[case] expected: &[&str]) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mod.rs"),
            "#[path = \"admin_impl.rs\"] pub mod admin; pub mod users;",
        )
        .unwrap();
        file_cache::bump_epoch();
        let segments: Vec<String> = segments.iter().map(ToString::to_string).collect();
        assert_eq!(declared_module_segments(dir.path(), &segments), expected);
    }

    #[test]
    fn folder_with_dot_declared_in_sibling_file_with_full_name() {
        // `api.v2` is declared by `api.v2.rs`; `with_extension` would look
        // for `api.rs`.
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("api.v2");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            dir.path().join("api.v2.rs"),
            "#[cfg(feature = \"admin\")] mod admin;",
        )
        .unwrap();
        std::fs::write(dir.path().join("api.rs"), "mod admin;").unwrap();
        file_cache::bump_epoch();
        assert_eq!(
            module_cfg_predicates(&folder, &["admin".to_string()]),
            ["feature = \"admin\""]
        );
    }

    #[test]
    fn nested_module_declared_in_sibling_file() {
        let dir = tempfile::tempdir().unwrap();
        let routes = dir.path().join("routes");
        std::fs::create_dir_all(routes.join("admin")).unwrap();
        std::fs::write(routes.join("mod.rs"), "pub mod admin;").unwrap();
        std::fs::write(
            routes.join("admin.rs"),
            "#[cfg(feature = \"x\")] pub mod users;",
        )
        .unwrap();
        file_cache::bump_epoch();
        let segments = ["admin".to_string(), "users".to_string()];
        assert_eq!(
            module_cfg_predicates(&routes, &segments),
            ["feature = \"x\""]
        );
        assert_eq!(
            module_cfg_predicates(&routes, &segments[..1]),
            Vec::<String>::new()
        );
    }

    #[rstest]
    #[case::top_level_only("#[route(get)] pub async fn a() {}", None)]
    #[case::inline("mod v2 { #[route(get)] pub async fn b() {} }", Some(("v2", "b")))]
    #[case::nested("mod v2 { mod inner { #[vespera::route] pub async fn c() {} } }", Some(("inner", "c")))]
    #[case::gated_skipped("#[cfg(test)] mod tests { #[route(get)] pub async fn t() {} }", None)]
    #[case::nested_in_gated_skipped(
        "#[cfg(test)] mod tests { mod fixtures { #[route(get)] pub async fn probe() {} } }",
        None
    )]
    #[case::non_route_fn("mod helpers { pub fn h() {} }", None)]
    #[case::declaration_only("mod other;", None)]
    fn finds_inline_module_routes(#[case] src: &str, #[case] expected: Option<(&str, &str)>) {
        let file: syn::File = syn::parse_str(src).unwrap();
        let found = inline_mod_route(&file.items, &|_| false);
        assert_eq!(
            found.as_ref().map(|(m, f)| (m.as_str(), f.as_str())),
            expected
        );
    }

    #[test]
    fn stored_inline_route_detection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("users.rs");
        std::fs::write(
            &file,
            "#[route(get)] pub async fn list() {}\n#[cfg(test)] mod tests { #[route(get)] pub async fn probe() {} }\nmod v2 { #[route(get)] pub async fn list() {} }",
        )
        .unwrap();
        let plain = dir.path().join("plain.rs");
        std::fs::write(&plain, "pub async fn list() {}").unwrap();
        file_cache::bump_epoch();

        // `list` is also a top-level route: the stored entry is that one.
        assert_eq!(stored_inline_mod_route(&file, &["list"], &[]), None);
        // `probe` is stored, so the gated test module was compiled.
        assert_eq!(
            stored_inline_mod_route(&file, &["list", "probe"], &[]),
            Some(("tests".to_string(), "probe".to_string()))
        );
        // A stored entry that records the inline module of `list` settles it
        // even though a top-level `list` exists (rust-analyzer path).
        assert_eq!(
            stored_inline_mod_route(&file, &["list"], &["list"]),
            Some(("v2".to_string(), "list".to_string()))
        );
        assert_eq!(stored_inline_mod_route(&plain, &["list"], &[]), None);
        assert_eq!(
            stored_inline_mod_route(&dir.path().join("missing.rs"), &["list"], &[]),
            None
        );
        // A first hit shadowed by a top-level handler never hides a later one.
        let shadowed = dir.path().join("shadowed.rs");
        std::fs::write(
            &shadowed,
            "#[route(get)] pub async fn list() {}\nmod a { #[route(get)] pub async fn list() {} }\nmod b { #[route(post)] pub async fn create() {} }",
        )
        .unwrap();
        file_cache::bump_epoch();
        assert_eq!(
            stored_inline_mod_route(&shadowed, &["list", "create"], &[]),
            Some(("b".to_string(), "create".to_string()))
        );
        // rust-analyzer still stores `moved` in `mod old` after it moved to
        // the top level: the IDE re-reads the file instead of trusting it.
        let moved = dir.path().join("moved.rs");
        std::fs::write(&moved, "#[route(get)] pub async fn moved() {}").unwrap();
        file_cache::bump_epoch();
        let stale = crate::route_impl::StoredRouteInfo {
            fn_name: "moved".to_string(),
            inline_module: Some("old".to_string()),
            ..Default::default()
        };
        let current = crate::route_impl::StoredRouteInfo {
            inline_module: Some(String::new()),
            ..stale.clone()
        };
        assert_eq!(
            stored_routes_inline_module(&moved, &[&stale, &current]),
            Some(("old".to_string(), "moved".to_string()))
        );
        // A nested stored scope reports its innermost module, like the
        // source scan (never the `v2::inner` path as a file name).
        let nested = crate::route_impl::StoredRouteInfo {
            inline_module: Some("v2::inner".to_string()),
            ..stale.clone()
        };
        assert_eq!(
            stored_routes_inline_module(&moved, &[&nested]),
            Some(("inner".to_string(), "moved".to_string()))
        );
        {
            let _ide = crate::macro_storage::test_ide::force();
            assert_eq!(
                stored_routes_inline_module(&moved, &[&stale, &current]),
                None
            );
        }
        let message = inline_mod_route_error(&file, &("v2".to_string(), "list".to_string()));
        assert!(
            message.contains("inline module `mod v2 { .. }`"),
            "{message}"
        );
        // Moving the handler into `v2.rs` would serve it at `/v2`, not
        // `/<file>`, so the advice must say the file move changes the URL.
        assert!(
            message.contains("to the top level of this file"),
            "{message}"
        );
        assert!(message.contains("change the route to `/v2`"), "{message}");
    }
}
