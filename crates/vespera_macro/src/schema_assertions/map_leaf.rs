//! `serde_json::Map` leaves of the Schema assertions (sidecar of
//! `schema_assertions`).

use std::cell::LazyCell;
use std::path::Path;

use syn::Type;

use crate::schema_macro::file_cache;

/// Whether the call-site file imports `Map` from serde_json, read on first use
/// (a derive without a bare `Map` leaf reads no file).
pub(super) type BareMapImport = LazyCell<bool, fn() -> bool>;

/// The lazy [`BareMapImport`] of one derive.
pub(super) fn bare_map_import() -> BareMapImport {
    LazyCell::new(call_site_imports_serde_json_map)
}

/// `serde_json::Map` (any path through a `serde_json` segment, `vespera`
/// re-export and the `serde_json::map::Map` module path included): documented
/// as a JSON object by the type-schema conversion, so it is exempt like the
/// `Value` leaf. A bare `Map` is that type only when the call-site file
/// imports it from serde_json (`use serde_json::Map;`, possibly inside a
/// `{..}` group or an inline module): otherwise it is a user type of that name
/// and keeps its `Schema` assertion (a forgotten derive is a build break, not a
/// silent `{"type": "object"}`).
pub(super) fn is_serde_json_map_leaf(ty: &Type, bare_map: &BareMapImport) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let segments = &type_path.path.segments;
    if segments.last().is_none_or(|last| last.ident != "Map") {
        return false;
    }
    if segments.len() == 1 {
        **bare_map
    } else {
        segments
            .iter()
            .rev()
            .skip(1)
            .any(|segment| segment.ident == "serde_json")
    }
}

/// Whether the file the derive is expanded in imports `Map` from serde_json.
fn call_site_imports_serde_json_map() -> bool {
    let file = proc_macro2::Span::call_site().local_file();
    imports_serde_json_map_at(file.as_deref())
}

/// [`call_site_imports_serde_json_map`] for the file at `path` (`None`: no
/// file is known, as in a unit test, so nothing is imported).
pub(super) fn imports_serde_json_map_at(path: Option<&Path>) -> bool {
    path.and_then(file_cache::get_parsed_file)
        .is_some_and(|file| items_import_serde_json_map(&file.items))
}

/// Whether `items` (a file's items, the inline modules' included) contain a
/// `use` that binds the name `Map` to serde_json's. A glob (`use
/// serde_json::*;`) and a renamed import (`use serde_json::Map as Json;`) do
/// not: the glob's `Map` is shadowed by any local one, and the renamed spelling
/// is not the bare `Map` leaf.
fn items_import_serde_json_map(items: &[syn::Item]) -> bool {
    items.iter().any(|item| match item {
        syn::Item::Use(item_use) => use_tree_imports_serde_json_map(&item_use.tree, false),
        syn::Item::Mod(module) => module
            .content
            .as_ref()
            .is_some_and(|(_, inner)| items_import_serde_json_map(inner)),
        _ => false,
    })
}

/// Whether `tree` binds `Map` from a path with a `serde_json` segment.
/// `through_serde_json` says whether a segment above `tree` is `serde_json`.
fn use_tree_imports_serde_json_map(tree: &syn::UseTree, through_serde_json: bool) -> bool {
    match tree {
        syn::UseTree::Path(path) => use_tree_imports_serde_json_map(
            &path.tree,
            through_serde_json || path.ident == "serde_json",
        ),
        syn::UseTree::Name(name) => through_serde_json && name.ident == "Map",
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .any(|tree| use_tree_imports_serde_json_map(tree, through_serde_json)),
        syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn parse_ty(src: &str) -> Type {
        syn::parse_str::<Type>(src).expect("valid type")
    }

    /// A lazy import that must never be read: qualified spellings decide alone.
    fn unread() -> BareMapImport {
        LazyCell::new(|| panic!("a qualified path does not read the file"))
    }

    fn imported() -> BareMapImport {
        LazyCell::new(|| true)
    }

    fn not_imported() -> BareMapImport {
        LazyCell::new(|| false)
    }

    #[test]
    fn serde_json_map_is_exempt_through_any_serde_json_path() {
        let unread = unread();
        assert!(is_serde_json_map_leaf(
            &parse_ty("serde_json::Map<String, u8>"),
            &unread
        ));
        assert!(is_serde_json_map_leaf(
            &parse_ty("vespera::serde_json::Map<String, serde_json::Value>"),
            &unread
        ));
        assert!(is_serde_json_map_leaf(
            &parse_ty("::serde_json::Map"),
            &unread
        ));
        assert!(is_serde_json_map_leaf(
            &parse_ty("serde_json::map::Map<String, u8>"),
            &unread
        ));
        assert!(!is_serde_json_map_leaf(&parse_ty("foo::Map"), &unread));
        assert!(!is_serde_json_map_leaf(
            &parse_ty("&serde_json::Map"),
            &unread
        ));
    }

    #[test]
    fn bare_map_is_exempt_only_when_the_file_imports_it() {
        assert!(is_serde_json_map_leaf(
            &parse_ty("Map<String, u8>"),
            &imported()
        ));
        assert!(!is_serde_json_map_leaf(
            &parse_ty("Map<String, u8>"),
            &not_imported()
        ));
    }

    #[test]
    fn bare_map_import_reads_no_file_without_a_call_site() {
        // Unit tests have no call-site file, so no import is found.
        assert!(!*bare_map_import());
    }

    #[rstest]
    #[case::plain("use serde_json::Map;", true)]
    #[case::group("use serde_json::{Map, Value};", true)]
    #[case::re_export("use vespera::serde_json::Map;", true)]
    #[case::leading_colons("use ::serde_json::Map;", true)]
    #[case::inline_module("mod models { use serde_json::Map; }", true)]
    #[case::user_map("use crate::game::Map;", false)]
    #[case::other_name_from_serde_json("use serde_json::Value;", false)]
    #[case::renamed("use serde_json::Map as Json;", false)]
    #[case::glob("use serde_json::*;", false)]
    #[case::declared_out_of_line("mod models;", false)]
    #[case::not_a_use("struct Map;", false)]
    fn import_of_map_from_serde_json(#[case] src: &str, #[case] imports: bool) {
        let file: syn::File = syn::parse_str(src).expect("valid file");
        assert_eq!(items_import_serde_json_map(&file.items), imports, "{src}");
    }

    #[test]
    fn call_site_file_is_read_for_its_import() {
        let dir = std::env::temp_dir();
        let serde_file = dir.join(format!("vespera-map-leaf-{}-serde.rs", std::process::id()));
        let user_file = dir.join(format!("vespera-map-leaf-{}-user.rs", std::process::id()));
        std::fs::write(&serde_file, "use serde_json::Map;\n").unwrap();
        std::fs::write(&user_file, "use crate::game::Map;\n").unwrap();
        assert!(imports_serde_json_map_at(Some(&serde_file)));
        assert!(!imports_serde_json_map_at(Some(&user_file)));
        assert!(!imports_serde_json_map_at(Some(&dir.join("missing.rs"))));
        assert!(!imports_serde_json_map_at(None));
        std::fs::remove_file(&serde_file).unwrap();
        std::fs::remove_file(&user_file).unwrap();
    }
}
