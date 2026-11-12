//! Which route files and handlers are actually part of the compiled crate.
//!
//! The collector walks the route folder on disk, but not every file there is
//! compiled: `#[cfg(feature = "admin")] mod admin;` removes `admin.rs` (and
//! everything below it) when the feature is off. `#[route]` records every
//! handler it expands into `ROUTE_STORAGE`, so a gated module that IS
//! compiled reaches the collector through the fast path. The slow path (a
//! file `#[route]` never registered) therefore skips files whose module
//! declaration chain carries `#[cfg(..)]`, and handlers that are themselves
//! `#[cfg(..)]`-gated — emitting `routes::admin::handler` for a module the
//! compiler removed would be a confusing "unresolved path" error.
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

/// Whether one of the `mod` declarations that make the route file with
/// module `segments` (relative to `folder_path`) part of the crate is
/// `#[cfg(..)]`-gated.
///
/// `segments == ["admin", "users"]` checks `mod admin;` (declared in
/// `<folder>/mod.rs` or `<folder>.rs`) and `mod users;` (declared in
/// `<folder>/admin/mod.rs` or `<folder>/admin.rs`).
pub(super) fn is_cfg_gated_module(folder_path: &Path, segments: &[String]) -> bool {
    let mut parent = folder_path.to_path_buf();
    for segment in segments {
        let declarers = [parent.join("mod.rs"), parent.with_extension("rs")];
        let gated = declarers.iter().any(|declarer| {
            file_cache::get_parsed_file(declarer).is_some_and(|file| {
                file.items.iter().any(|item| {
                    matches!(item, Item::Mod(module)
                        if module.ident == segment.as_str() && is_cfg_gated(&module.attrs))
                })
            })
        });
        if gated {
            return true;
        }
        parent.push(segment);
    }
    false
}

/// `(module, handler)` of the first `#[route]` handler declared inside an
/// inline `mod { .. }` block of `items` (nested blocks included). Gated
/// blocks (`#[cfg(test)] mod tests { .. }`) are skipped unless `include`
/// says the handler was compiled (it is in `ROUTE_STORAGE`).
pub(super) fn inline_mod_route(
    items: &[Item],
    include: &dyn Fn(&str) -> bool,
) -> Option<(String, String)> {
    items.iter().find_map(|item| {
        let Item::Mod(module) = item else {
            return None;
        };
        let (_, content) = module.content.as_ref()?;
        let gated = is_cfg_gated(&module.attrs);
        content
            .iter()
            .find_map(|inner| match inner {
                Item::Fn(handler)
                    if extract_route_info(&handler.attrs).is_some()
                        && (!gated || include(&handler.sig.ident.to_string())) =>
                {
                    Some((module.ident.to_string(), handler.sig.ident.to_string()))
                }
                _ => None,
            })
            .or_else(|| inline_mod_route(content, include))
    })
}

/// The compile error for a handler found by [`inline_mod_route`].
pub(super) fn inline_mod_route_error(file: &Path, (module, handler): &(String, String)) -> String {
    format!(
        "vespera! macro: `#[route]` handler `{handler}` is declared inside the inline module `mod {module} {{ .. }}` in '{}'. Route handlers must be top-level functions of a route file — move `{handler}` out of `mod {module}`, or into its own file `{module}.rs` next to this one.",
        file.display()
    )
}

/// Whether the fast-path `file` declares one of its stored handlers inside an
/// inline module. Only files whose text contains an inline `mod` are parsed.
pub(super) fn stored_inline_mod_route(
    file: &Path,
    stored_names: &[&str],
) -> Option<(String, String)> {
    let content = file_cache::get_file_content(file)?;
    if !content.contains("mod ") || !content.contains("#[") {
        return None;
    }
    let ast = file_cache::get_parsed_file(file)?;
    let top_level = |name: &str| {
        ast.items.iter().any(
            |item| matches!(item, Item::Fn(handler) if handler.sig.ident == name && extract_route_info(&handler.attrs).is_some()),
        )
    };
    inline_mod_route(&ast.items, &|name| stored_names.contains(&name))
        .filter(|(_, handler)| stored_names.contains(&handler.as_str()) && !top_level(handler))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain_module("mod admin;", &["admin"], false)]
    #[case::gated_module("#[cfg(feature = \"admin\")] mod admin;", &["admin"], true)]
    #[case::other_module_gated("#[cfg(test)] mod tests; mod admin;", &["admin"], false)]
    #[case::folder_root(r#"#[cfg(feature = "x")] mod admin;"#, &[], false)]
    fn module_declaration_cfg(
        #[case] mod_rs: &str,
        #[case] segments: &[&str],
        #[case] expected: bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.rs"), mod_rs).unwrap();
        file_cache::bump_epoch();
        let segments: Vec<String> = segments.iter().map(ToString::to_string).collect();
        assert_eq!(is_cfg_gated_module(dir.path(), &segments), expected);
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
        assert!(is_cfg_gated_module(&routes, &segments));
        assert!(!is_cfg_gated_module(&routes, &segments[..1]));
    }

    #[rstest]
    #[case::top_level_only("#[route(get)] pub async fn a() {}", None)]
    #[case::inline("mod v2 { #[route(get)] pub async fn b() {} }", Some(("v2", "b")))]
    #[case::nested("mod v2 { mod inner { #[vespera::route] pub async fn c() {} } }", Some(("inner", "c")))]
    #[case::gated_skipped("#[cfg(test)] mod tests { #[route(get)] pub async fn t() {} }", None)]
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
        assert_eq!(stored_inline_mod_route(&file, &["list"]), None);
        // `probe` is stored, so the gated test module was compiled.
        assert_eq!(
            stored_inline_mod_route(&file, &["list", "probe"]),
            Some(("tests".to_string(), "probe".to_string()))
        );
        assert_eq!(stored_inline_mod_route(&plain, &["list"]), None);
        assert_eq!(
            stored_inline_mod_route(&dir.path().join("missing.rs"), &["list"]),
            None
        );
        let message = inline_mod_route_error(&file, &("v2".to_string(), "list".to_string()));
        assert!(
            message.contains("inline module `mod v2 { .. }`"),
            "{message}"
        );
    }
}
