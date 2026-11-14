use rstest::rstest;
use tempfile::TempDir;

use super::*;
use crate::schema_macro::type_utils::resolve_type_to_absolute_path;

/// A temp crate: `files` are written below `src/`, `CARGO_MANIFEST_DIR`
/// points at it for the returned guard's lifetime.
fn temp_crate(files: &[(&str, &str)]) -> (TempDir, crate::env_access::test_env::EnvGuard) {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        let path = dir.path().join("src").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    file_cache::bump_epoch();
    (dir, guard)
}

fn segs(path: &str) -> Vec<String> {
    path.split("::").map(str::to_string).collect()
}

fn struct_names(items: &[syn::Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(s) => Some(s.ident.to_string()),
            _ => None,
        })
        .collect()
}

const TREE: &[(&str, &str)] = &[
    (
        "lib.rs",
        r#"
pub struct RootModel { pub id: i32 }
pub mod models;
#[path = "custom/place.rs"]
pub mod placed;
pub mod inline_root {
    pub struct InRoot { pub id: i32 }
    pub mod deeper;
    #[path = "elsewhere.rs"]
    pub mod moved;
}
"#,
    ),
    (
        "models/mod.rs",
        "pub mod user; pub mod nested { pub struct Inner { pub v: i32 } }",
    ),
    (
        "models/user.rs",
        "pub struct Model { pub id: i32 } pub mod child;",
    ),
    ("models/user/child.rs", "pub struct Child { pub id: i32 }"),
    (
        "custom/place.rs",
        "pub struct Placed { pub id: i32 } pub mod sub;",
    ),
    ("custom/sub.rs", "pub struct PlacedSub { pub id: i32 }"),
    ("inline_root/deeper.rs", "pub struct Deeper { pub id: i32 }"),
    (
        "inline_root/elsewhere.rs",
        "pub struct Moved { pub id: i32 }",
    ),
];

#[rstest]
#[case::crate_root("crate", "RootModel")]
#[case::file_module("crate::models::user", "Model")]
#[case::file_module_without_crate("models::user", "Model")]
#[case::inline_in_mod_rs("crate::models::nested", "Inner")]
#[case::non_mod_rs_child("crate::models::user::child", "Child")]
#[case::path_attr("crate::placed", "Placed")]
#[case::path_attr_child("crate::placed::sub", "PlacedSub")]
#[case::inline_in_root("crate::inline_root", "InRoot")]
#[case::file_under_inline("crate::inline_root::deeper", "Deeper")]
#[case::path_attr_in_inline("crate::inline_root::moved", "Moved")]
fn module_items_follow_the_module_tree(#[case] module: &str, #[case] expected: &str) {
    let (_dir, _guard) = temp_crate(TREE);
    let items = module_items(&segs(module)).expect("module resolves");
    assert_eq!(struct_names(&items), [expected]);
}

#[rstest]
#[case::missing_module("crate::nope")]
#[case::relative("crate::super::x")]
#[case::missing_inline_under_file("crate::models::user::nope")]
fn module_items_misses(#[case] module: &str) {
    let (_dir, _guard) = temp_crate(TREE);
    assert!(module_items(&segs(module)).is_none());
}

#[test]
fn module_items_without_manifest_dir_is_none() {
    let _guard = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
    file_cache::bump_epoch();
    assert!(module_items(&segs("crate::models")).is_none());
}

#[test]
fn binary_crate_root_prefers_main_rs() {
    let (_dir, _guard) = temp_crate(&[
        ("lib.rs", "pub struct FromLib;"),
        ("main.rs", "pub struct FromMain;"),
    ]);
    assert_eq!(
        struct_names(&module_items(&segs("crate")).unwrap()),
        ["FromLib"]
    );
    let _bin = crate::env_access::test_env::set_var("CARGO_BIN_NAME", "app");
    assert_eq!(
        struct_names(&module_items(&segs("crate")).unwrap()),
        ["FromMain"]
    );
}

#[test]
fn struct_lookups_in_items() {
    let file: syn::File = syn::parse_str(
        "pub struct A { x: i32 } mod one { pub struct B; } mod two { pub struct B; mod three { pub struct C; } } mod ext;",
    )
    .unwrap();
    assert!(
        struct_in_items(&file.items, "A")
            .unwrap()
            .contains("struct A")
    );
    assert!(struct_in_items(&file.items, "C").is_none());
    let (def, chain) = unique_nested_struct(&file.items, "C").unwrap();
    assert!(def.contains("struct C"));
    assert_eq!(chain, ["two", "three"]);
    // Two candidates are ambiguous, none is a miss.
    assert!(unique_nested_struct(&file.items, "B").is_none());
    assert!(unique_nested_struct(&file.items, "Z").is_none());
}

const SCOPE_FILE: &str = r"
use std::collections::HashMap as Map;
use super::enums::{self, Status};
use self::local_mod::Thing as Renamed;
use crate::missing::*;
use crate::shared::*;
use sea_orm::entity::prelude::*;
use ::external::Item;
use child::Grand;
use unknown_crate::Other;
use something::Hidden as _;
use super::super::top::Top;
pub mod child { pub struct Grand; }
pub mod local_mod { pub struct Thing; }
pub struct LocalStruct;
pub enum LocalEnum { A }
pub union LocalUnion { a: u8 }
pub type Alias = u8;
pub trait LocalTrait {}
pub fn local_fn() {}
pub const LOCAL_CONST: u8 = 1;
pub static LOCAL_STATIC: u8 = 1;
pub struct Value;
macro_rules! m { () => {} }
";

#[test]
fn module_scope_collects_imports_locals_and_globs() {
    let (_dir, _guard) = temp_crate(&[("models/memo.rs", SCOPE_FILE)]);
    let scope = module_scope(&segs("crate::models::memo")).expect("scope");
    let import = |name: &str| scope.imports.get(name).map(|p| p.join("::"));
    assert_eq!(import("Map").as_deref(), Some("std::collections::HashMap"));
    assert_eq!(import("enums").as_deref(), Some("crate::models::enums"));
    assert_eq!(
        import("Status").as_deref(),
        Some("crate::models::enums::Status")
    );
    assert_eq!(
        import("Renamed").as_deref(),
        Some("crate::models::memo::local_mod::Thing")
    );
    assert_eq!(import("Item").as_deref(), Some("::external::Item"));
    assert_eq!(
        import("Grand").as_deref(),
        Some("crate::models::memo::child::Grand")
    );
    assert_eq!(import("Other").as_deref(), Some("unknown_crate::Other"));
    assert_eq!(import("Top").as_deref(), Some("crate::top::Top"));
    assert!(import("_").is_none());
    for local in [
        "child",
        "local_mod",
        "LocalStruct",
        "LocalEnum",
        "LocalUnion",
        "Alias",
        "LocalTrait",
        "local_fn",
        "LOCAL_CONST",
        "LOCAL_STATIC",
        "Value",
    ] {
        assert!(scope.locals.contains(local), "{local}");
    }
    assert!(scope.has_sea_orm_glob());
    assert_eq!(scope.globs.len(), 3);
    // Cached for the rest of the invocation.
    assert!(Arc::ptr_eq(
        &scope,
        &module_scope(&segs("crate::models::memo")).unwrap()
    ));
}

#[test]
fn module_scope_rejects_non_crate_paths_and_misses() {
    let (_dir, _guard) = temp_crate(&[]);
    assert!(module_scope(&segs("models::memo")).is_none());
    assert!(module_scope(&[]).is_none());
    assert!(module_scope(&segs("crate::missing")).is_none());
}

#[test]
fn use_self_rename_and_group_self() {
    let (_dir, _guard) = temp_crate(&[(
        "a.rs",
        "use super::b::{self as bee}; use crate::c::{self}; use self as _;",
    )]);
    let scope = module_scope(&segs("crate::a")).unwrap();
    assert_eq!(scope.imports["bee"].join("::"), "crate::b");
    assert_eq!(scope.imports["c"].join("::"), "crate::c");
}

const RESOLVE_TREE: &[(&str, &str)] = &[
    (
        "models/memo.rs",
        r"
use super::enums::Status;
use super::enums;
use crate::missing::*;
use crate::shared::*;
use sea_orm::entity::prelude::*;
use other_crate::Json;
pub struct Local;
pub mod kinds { pub enum Kind { A } }
",
    ),
    (
        "models/enums.rs",
        "pub enum Status { A } pub enum Other { B }",
    ),
    (
        "shared.rs",
        "pub struct Shared; pub use crate::deep::Deep; use crate::deep::Hidden;",
    ),
    ("deep.rs", "pub struct Deep; pub struct Hidden;"),
    (
        "plain.rs",
        "pub struct Value; pub struct Plain { pub v: Value }",
    ),
];

#[rstest]
#[case::private_import("Status", "crate :: models :: enums :: Status")]
#[case::local_item("Local", "crate :: models :: memo :: Local")]
#[case::module_import_multi_segment("enums::Other", "crate :: models :: enums :: Other")]
#[case::child_module_multi_segment("kinds::Kind", "crate :: models :: memo :: kinds :: Kind")]
#[case::super_relative("super::enums::Other", "crate :: models :: enums :: Other")]
#[case::self_relative("self::Local", "crate :: models :: memo :: Local")]
#[case::crate_glob("Shared", "crate :: shared :: Shared")]
#[case::crate_glob_reexport("Deep", "crate :: deep :: Deep")]
#[case::imported_json_is_not_serde_json("Json", "other_crate :: Json")]
#[case::sea_orm_date("Date", "vespera :: chrono :: NaiveDate")]
#[case::sea_orm_time("Time", "vespera :: chrono :: NaiveTime")]
#[case::sea_orm_datetime("DateTime", "vespera :: chrono :: NaiveDateTime")]
#[case::sea_orm_uuid("Uuid", "sea_orm :: entity :: prelude :: Uuid")]
#[case::sea_orm_decimal("Decimal", "sea_orm :: entity :: prelude :: Decimal")]
#[case::nested(
    "Option<Vec<Status>>",
    "Option < Vec < crate :: models :: enums :: Status > >"
)]
#[case::unknown_falls_back_to_module("Mystery", "crate :: models :: memo :: Mystery")]
#[case::external_multi_segment("chrono::NaiveDate", "chrono :: NaiveDate")]
fn resolve_through_module_scope(#[case] input: &str, #[case] expected: &str) {
    let (_dir, _guard) = temp_crate(RESOLVE_TREE);
    let ty: syn::Type = syn::parse_str(input).unwrap();
    let module = segs("crate::models::memo");
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &module).to_string(),
        expected
    );
}

#[test]
fn local_value_type_is_not_serde_json() {
    let (_dir, _guard) = temp_crate(RESOLVE_TREE);
    let ty: syn::Type = syn::parse_str("Value").unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &segs("crate::plain")).to_string(),
        "crate :: plain :: Value"
    );
}

#[test]
fn imported_sea_orm_alias_is_converted() {
    let (_dir, _guard) = temp_crate(&[(
        "m.rs",
        "use sea_orm::prelude::DateTimeUtc; use sea_orm::prelude::Uuid;",
    )]);
    let module = segs("crate::m");
    let ty: syn::Type = syn::parse_str("DateTimeUtc").unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &module).to_string(),
        "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
    );
    let ty: syn::Type = syn::parse_str("Uuid").unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &module).to_string(),
        "sea_orm :: prelude :: Uuid"
    );
}

#[rstest]
#[case::empty(&[], Resolved::Unknown)]
#[case::crate_path(&["crate", "x"], Resolved::Path(vec!["crate".into(), "x".into()]))]
#[case::leading_colon(&["", "x"], Resolved::Path(vec![String::new(), "x".into()]))]
fn resolve_name_without_scope(#[case] path: &[&str], #[case] expected: Resolved) {
    let path: Vec<String> = path.iter().map(ToString::to_string).collect();
    assert_eq!(resolve_name(&path, &[]), expected);
    // `self` / `super` need an absolute module.
    assert_eq!(
        resolve_name(&segs("self::x"), &segs("models")),
        Resolved::Unknown
    );
}

#[test]
fn glob_without_match_is_unknown() {
    let (_dir, _guard) = temp_crate(&[("a.rs", "use crate::b::*;"), ("b.rs", "use crate::a::*;")]);
    assert_eq!(
        resolve_name(&segs("Nothing"), &segs("crate::a")),
        Resolved::Unknown
    );
}

#[rstest]
#[case::plain(&["crate", "a", "B"], "crate :: a :: B")]
#[case::leading(&["", "ext", "B"], ":: ext :: B")]
#[case::keyword(&["crate", "type", "B"], "crate :: r#type :: B")]
#[case::raw(&["crate", "r#match"], "crate :: r#match")]
#[case::self_root(&["self", "B"], "self :: B")]
fn path_tokens_render(#[case] path: &[&str], #[case] expected: &str) {
    let path: Vec<String> = path.iter().map(ToString::to_string).collect();
    assert_eq!(path_tokens(&path).to_string(), expected);
}

#[test]
fn path_attr_requires_string_literal() {
    let item: syn::ItemMod = syn::parse_str("#[path = 1] #[doc = \"x\"] #[cfg(x)] mod m;").unwrap();
    assert!(path_attr(&item.attrs).is_none());
}
