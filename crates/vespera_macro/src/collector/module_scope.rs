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
//!
//! The rejection is decided from the source shape, so two `cfg` cases are
//! deliberate, not feature-aware:
//!
//! - A handler carrying its own `#[cfg(..)]` inside an inline module is
//!   rejected even in a build where that `cfg` is off (a `#[cfg(feature =
//!   "beta")] #[route(get)]` handler in `mod v2 { .. }` errors without
//!   `beta`). The rule does not evaluate features, so it holds in every build
//!   (`tests_inline_cfg`).
//! - A route file gated by a module declaration's `#[cfg(..)]` or an inner
//!   `#![cfg(..)]` takes the slow path, which records only its top-level
//!   handlers and never runs this check (`collector.rs`, the
//!   `!module_cfg.is_empty()` branch). An inline module there is skipped, not
//!   rejected: the gate may compile the whole file out, and rustc never reads
//!   a compiled-out file. The fast path (a `#[route]` storage entry for the
//!   file) still rejects a stored handler whose inline module is recorded
//!   (`stored_routes_inline_module`).

use std::path::Path;

use quote::ToTokens;
use syn::{Item, punctuated::Punctuated};

use crate::{
    route::extract_route_info,
    schema_macro::{
        cfg_eval::{Predicate, statically_active, statically_false},
        file_cache,
    },
};

/// Whether `attrs` gate the item by a `#[cfg(..)]`, directly or through a
/// `cfg_attr` (see [`cfg_predicates`]).
pub(super) fn is_cfg_gated(attrs: &[syn::Attribute]) -> bool {
    !cfg_predicates(attrs).is_empty()
}

/// The predicates (token text) under which an item carrying `attrs` is
/// compiled: its `#[cfg(..)]`s, and the `cfg(q)`s of its `cfg_attr(p, ..)`s
/// as `any(not(p), q)` (the compiler keeps the item under that; a statically
/// true `p` leaves `q`, a statically false one contributes nothing). A
/// `cfg_attr` without a `cfg` (`cfg_attr(p, derive(..))`) gates nothing.
pub(super) fn cfg_predicates(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut predicates = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("cfg") {
            if let Ok(list) = attr.meta.require_list() {
                predicates.push(list.tokens.to_string());
            }
        } else if attr.path().is_ident("cfg_attr")
            && let Ok((predicate, metas)) = attr
                .meta
                .require_list()
                .and_then(|list| list.parse_args_with(parse_cfg_attr_args))
        {
            cfg_attr_predicates(&predicate, &metas, &mut predicates);
        }
    }
    predicates
}

/// The `(predicate, metas)` of a `cfg_attr(predicate, meta, ..)` argument list.
fn parse_cfg_attr_args(
    input: syn::parse::ParseStream<'_>,
) -> syn::Result<(Predicate, Punctuated<syn::Meta, syn::Token![,]>)> {
    let predicate = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    let metas = Punctuated::parse_terminated(input)?;
    Ok((predicate, metas))
}

/// Pushes the `cfg(q)` predicates of `cfg_attr(predicate, metas..)` into
/// `out`, nested `cfg_attr(inner, ..)`s included (under `all(predicate,
/// inner)`).
fn cfg_attr_predicates(
    predicate: &Predicate,
    metas: &Punctuated<syn::Meta, syn::Token![,]>,
    out: &mut Vec<String>,
) {
    if statically_false(predicate) {
        return;
    }
    for meta in metas {
        let syn::Meta::List(list) = meta else {
            continue;
        };
        if list.path.is_ident("cfg") {
            if let Ok(condition) = list.parse_args::<Predicate>() {
                out.push(gate_under(predicate, &condition));
            }
        } else if list.path.is_ident("cfg_attr")
            && let Ok((inner, inner_metas)) = list.parse_args_with(parse_cfg_attr_args)
        {
            let combined: Predicate = syn::parse_quote!(all(#predicate, #inner));
            cfg_attr_predicates(&combined, &inner_metas, out);
        }
    }
}

/// `condition` as the predicate the item is compiled under when
/// `predicate` gates it: `condition` itself when `predicate` always holds,
/// else `any(not(predicate), condition)`.
fn gate_under(predicate: &Predicate, condition: &Predicate) -> String {
    if statically_active(predicate) {
        condition.to_token_stream().to_string()
    } else {
        let gate: Predicate = syn::parse_quote!(any(not(#predicate), #condition));
        gate.to_token_stream().to_string()
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
                        .then(|| (syn::ext::IdentExt::unraw(&module.ident).to_string(), name))
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
                    out.push((
                        syn::ext::IdentExt::unraw(ident).to_string(),
                        handler.sig.ident.to_string(),
                    ));
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
    // A raw identifier names the module without its `r#` (the advice would
    // otherwise say `r#b.rs` / `/r#b`).
    #[case::raw_module("mod r#b { #[route(get)] pub async fn list() {} }", Some(("b", "list")))]
    #[case::raw_nested_module(
        "mod v2 { mod r#inner { #[route(get)] pub async fn c() {} } }",
        Some(("inner", "c"))
    )]
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

    /// The fast-path scan names a raw inline module without its `r#`, like
    /// the source scan does, so the advice tells the user the real file name.
    #[test]
    fn stored_inline_raw_module_is_named_without_its_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("users.rs");
        std::fs::write(&file, "mod r#b { #[route(get)] pub async fn list() {} }").unwrap();
        file_cache::bump_epoch();
        let found = stored_inline_mod_route(&file, &["list"], &[]);
        assert_eq!(found, Some(("b".to_string(), "list".to_string())));
        let message = inline_mod_route_error(&file, &found.unwrap());
        assert!(message.contains("mod b { .. }"), "{message}");
        assert!(message.contains("change the route to `/b`"), "{message}");
    }

    /// The `cfg_predicates` of `source` (outer attributes only), without
    /// spaces so the expectations do not depend on token-printing spacing.
    fn attr_cfgs(source: &str) -> Vec<String> {
        let attrs = syn::parse::Parser::parse_str(syn::Attribute::parse_outer, source).unwrap();
        cfg_predicates(&attrs)
            .into_iter()
            .map(|predicate| predicate.replace(' ', ""))
            .collect()
    }

    /// A `cfg_attr(p, cfg(q))` gates a handler (round-32 macrorest-1): rustc
    /// keeps it under `any(not(p), q)`, folded to `q` when `p` always holds,
    /// and dropped when `p` never does. Other `cfg_attr` metas gate nothing.
    #[rstest]
    #[case::cfg_attr_wrapped(
        "#[cfg_attr(feature = \"beta\", cfg(unix))] #[route(post)]",
        &["any(not(feature=\"beta\"),unix)"]
    )]
    #[case::statically_true_folds("#[cfg_attr(all(), cfg(test))]", &["test"])]
    #[case::literal_true_folds("#[cfg_attr(true, cfg(unix))]", &["unix"])]
    #[case::statically_false_drops("#[cfg_attr(false, cfg(unix))]", &[])]
    #[case::cfg_attr_without_cfg("#[cfg_attr(unix, derive(Debug))]", &[])]
    #[case::non_list_meta("#[cfg_attr(feature = \"x\", must_use)]", &[])]
    #[case::unparseable_cfg("#[cfg_attr(unix, cfg(= 1))]", &[])]
    #[case::unparseable_cfg_attr("#[cfg_attr(= 1)]", &[])]
    #[case::nested_cfg_attr(
        "#[cfg_attr(unix, cfg_attr(test, cfg(feature = \"x\")))]",
        &["any(not(all(unix,test)),feature=\"x\")"]
    )]
    #[case::direct_cfg_kept("#[cfg(unix)] #[cfg_attr(feature = \"x\", cfg(test))]", &["unix", "any(not(feature=\"x\"),test)"])]
    fn cfg_attr_gates_items(#[case] source: &str, #[case] expected: &[&str]) {
        assert_eq!(attr_cfgs(source), expected);
        assert_eq!(is_cfg_gated_source(source), !expected.is_empty());
    }

    fn is_cfg_gated_source(source: &str) -> bool {
        let attrs = syn::parse::Parser::parse_str(syn::Attribute::parse_outer, source).unwrap();
        is_cfg_gated(&attrs)
    }

    /// A handler wrapped by `cfg_attr` reaches the collector's `cfg_routes`
    /// through `cfg_predicates` (the call site in `collector.rs`).
    #[test]
    fn cfg_attr_wrapped_handler_is_gated() {
        let file: syn::File = syn::parse_str(
            "#[cfg_attr(feature = \"beta\", cfg(unix))] #[route(post)] pub async fn beta() {}",
        )
        .unwrap();
        let Item::Fn(handler) = &file.items[0] else {
            panic!("expected a fn item");
        };
        assert!(extract_route_info(&handler.attrs).is_some());
        assert_eq!(
            cfg_predicates(&handler.attrs)
                .into_iter()
                .map(|predicate| predicate.replace(' ', ""))
                .collect::<Vec<_>>(),
            ["any(not(feature=\"beta\"),unix)"]
        );
    }
}
