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
use sea_orm as orm;
use enums::Other as Chained;
use sub::Sub as ViaGlob;
use crate::shared::Shared as Twin;
#[cfg(any())]
use crate::deep::Deep as Twin;
#[cfg(false)]
pub struct Local;
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
        "pub struct Shared; pub use crate::deep::Deep; use crate::deep::Hidden; pub mod sub { pub struct Sub; }",
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
// Anything else unresolved comes from the SeaORM prelude glob (through
// the module it would be a private glob import).
#[case::unknown_in_sea_orm_module("Mystery", "sea_orm :: entity :: prelude :: Mystery")]
#[case::sea_orm_time_crate_alias("TimeDateTime", "sea_orm :: entity :: prelude :: TimeDateTime")]
#[case::sea_orm_chrono_alias(
    "ChronoDateTimeUtc",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
)]
#[case::sea_orm_chrono_tz_alias(
    "ChronoDateTimeWithTimeZone",
    "vespera :: chrono :: DateTime < vespera :: chrono :: FixedOffset >"
)]
#[case::sea_orm_chrono_local_alias(
    "ChronoDateTimeLocal",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Local >"
)]
#[case::sea_orm_chrono_naive_alias("ChronoDateTime", "vespera :: chrono :: NaiveDateTime")]
#[case::sea_orm_chrono_date_alias("ChronoDate", "vespera :: chrono :: NaiveDate")]
#[case::sea_orm_chrono_time_alias("ChronoTime", "vespera :: chrono :: NaiveTime")]
#[case::sea_orm_chrono_utc_alias("ChronoUtc", "vespera :: chrono :: Utc")]
// A qualified alias through an import of the SeaORM crate (`use sea_orm as orm`).
#[case::imported_alias_crate(
    "orm::prelude::DateTimeUtc",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
)]
// A glob brings in only the glob module's PUBLIC names.
#[case::crate_glob_private_import_hidden("Hidden", "sea_orm :: entity :: prelude :: Hidden")]
// `use enums::Other` where `enums` is itself an import.
#[case::chained_import("Chained", "crate :: models :: enums :: Other")]
// `use sub::Sub` where `sub` came in through a glob.
#[case::import_through_glob("ViaGlob", "crate :: shared :: sub :: Sub")]
#[case::multi_segment_through_glob("sub::Sub", "crate :: shared :: sub :: Sub")]
// `#[cfg]`-dropped imports / items name nothing.
#[case::cfg_twin_import("Twin", "crate :: shared :: Shared")]
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
fn unknown_name_without_sea_orm_glob_falls_back_to_module() {
    let (_dir, _guard) = temp_crate(RESOLVE_TREE);
    let ty: syn::Type = syn::parse_str("Mystery").unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &segs("crate::plain")).to_string(),
        "crate :: plain :: Mystery"
    );
}

/// Inside the glob module's own subtree, its private names are visible.
#[test]
fn glob_private_names_are_visible_to_descendants() {
    let (_dir, _guard) = temp_crate(&[
        ("a.rs", "use crate::deep::Thing; pub mod b;"),
        ("a/b.rs", "use super::*;"),
        ("deep.rs", "pub struct Thing;"),
    ]);
    assert_eq!(
        resolve_name(&["Thing".to_string()], &segs("crate::a::b")),
        Resolved::Path(segs("crate::deep::Thing"))
    );
}

/// A self-referencing / cyclic import chain stops instead of looping.
#[test]
fn cyclic_import_chain_terminates() {
    let (_dir, _guard) = temp_crate(&[("c.rs", "use foo::foo; use x::y as z; use z::w as x;")]);
    let scope = module_scope(&segs("crate::c")).unwrap();
    assert_eq!(scope.imports["foo"].join("::"), "foo::foo");
    assert!(scope.imports["x"].len() <= 8);
}

/// A `#[cfg]`-dropped `#[path]` module twin is not descended into.
#[test]
fn cfg_dropped_module_twin_is_skipped() {
    let (_dir, _guard) = temp_crate(&[
        (
            "lib.rs",
            "#[cfg(any())]\n#[path = \"pg/models.rs\"]\npub mod models;\n#[path = \"lite/models.rs\"]\npub mod models;",
        ),
        ("pg/models.rs", "pub struct Pg;"),
        ("lite/models.rs", "pub struct Lite;"),
    ]);
    let items = module_items(&segs("crate::models")).unwrap();
    assert!(struct_in_items(&items, "Lite").is_some());
    assert!(struct_in_items(&items, "Pg").is_none());
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

/// L1 (round 12): a name a crate-internal glob brings in wins over the
/// alias table (`Value` used to become `serde_json::Value`); a SeaORM name
/// the glob module imports is still converted.
#[rstest]
#[case::glob_defined_value("Value", "crate :: types :: Value")]
#[case::glob_defined_json("Json", "crate :: types :: Json")]
#[case::glob_imported_sea_orm_alias(
    "DateTimeUtc",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
)]
#[case::glob_imported_sea_orm_plain("Uuid", "sea_orm :: prelude :: Uuid")]
#[case::not_in_glob("JsonValue", "vespera :: serde_json :: Value")]
fn crate_glob_names_win_over_aliases(#[case] input: &str, #[case] expected: &str) {
    let (_dir, _guard) = temp_crate(&[
        ("globbed.rs", "use crate::types::*;"),
        (
            "types.rs",
            "pub struct Value; pub struct Json; pub use sea_orm::prelude::DateTimeUtc; pub use sea_orm::prelude::Uuid;",
        ),
    ]);
    let ty: syn::Type = syn::parse_str(input).unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &segs("crate::globbed")).to_string(),
        expected
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

/// `item_module_chain` finds structs and enums inside inline modules, picks
/// a repeated name by its line, and falls back to the top level.
#[rstest]
#[case::enum_in_inline_mod("Kind", 0, &["a"])]
#[case::struct_in_nested_mod("Row", 0, &["a", "b"])]
#[case::repeated_name_by_line("Dup", 5, &["b2"])]
#[case::repeated_name_wrong_line("Dup", 99, &[])]
#[case::missing("Nope", 0, &[])]
fn item_module_chain_walks_inline_modules(
    #[case] name: &str,
    #[case] line: usize,
    #[case] expected: &[&str],
) {
    let file = syn::parse_file(
        "mod a {\n    pub enum Kind { A }\n    mod b { pub struct Row; }\n}\nmod b2 { pub struct Dup; }\nmod b3 { pub struct Dup; }\nmod decl;\nfn f() {}\n",
    )
    .unwrap();
    assert_eq!(item_module_chain(&file.items, name, line), expected);
}

/// `item_module_chain_cached` reads real line positions off the file's
/// fallback-parsed line index (a compiler parse puts every re-parsed token on
/// the call site's line), so a repeated name is told apart by line.
#[rstest]
#[case::repeated_name("Dup", 5, true, Some(&["b2"][..]))]
#[case::repeated_name_other_line("Dup", 6, true, Some(&["b3"][..]))]
#[case::repeated_name_no_line("Dup", 0, true, Some(&[][..]))]
#[case::unique_name("Kind", 0, true, Some(&["a"][..]))]
#[case::unreadable_file("Dup", 5, false, None)]
fn item_module_chain_cached_tells_repeated_names_apart(
    #[case] name: &str,
    #[case] line: usize,
    #[case] write: bool,
    #[case] expected: Option<&[&str]>,
) {
    let source = "mod a {\n    pub enum Kind { A }\n}\n\nmod b2 { pub struct Dup; }\nmod b3 { pub struct Dup; }\n";
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    if write {
        std::fs::write(&file, source).unwrap();
    }
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(
        item_module_chain_cached(&file, name, line),
        expected.map(|chain| chain.iter().map(ToString::to_string).collect())
    );
}

/// A file that does not parse has no line index.
#[test]
fn item_module_chain_cached_unparsable_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "mod {{{").unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(item_module_chain_cached(&file, "Dup", 1), None);
    assert_eq!(inline_chain_at(&file, 1), Vec::<String>::new());
}

/// The direct construction of a plain segment matches what syn's parser
/// (the previous path for every segment) builds: keywords raw, the rest
/// plain; other text still goes through the parser.
#[rstest]
#[case::plain("user", "user")]
#[case::raw_input("r#type", "r#type")]
#[case::keyword("type", "r#type")]
#[case::reserved("yield", "r#yield")]
// L7 (round 13): `gen` is reserved in edition 2024 (`r#gen` is valid in
// every edition).
#[case::edition_word("gen", "r#gen")]
#[case::root("self", "self")]
#[case::underscored("_private", "_private")]
#[case::non_ascii("caf\u{e9}", "caf\u{e9}")]
fn segment_ident_builds_plain_and_raw_idents(#[case] segment: &str, #[case] expected: &str) {
    assert_eq!(segment_ident(segment).to_string(), expected);
}

/// L6 (round 13): a `main.rs` / `lib.rs` below the crate root is a plain
/// module file: its inline `mod inner { pub mod y; }` lives in
/// `main/inner/y.rs` (it was looked up in `inner/y.rs`, like a `mod.rs`).
/// R22-SM3-02: `::entities::bakery` names an external crate, not a module of
/// this one, so it has no crate-relative location (it used to be probed as a
/// rooted `/entities/bakery.rs`).
#[test]
fn crate_relative_rejects_an_external_leading_colon_path() {
    assert!(crate_relative(&segs("::entities::bakery")).is_none());
    assert!(crate_relative(&segs("crate::entities::bakery")).is_some());
}

#[test]
fn nested_main_rs_is_not_a_mod_rs() {
    let (_dir, _guard) = temp_crate(&[
        ("lib.rs", "pub mod models;"),
        ("models.rs", "pub mod main;"),
        ("models/main.rs", "pub mod inner { pub mod y; }"),
        ("models/main/inner/y.rs", "pub struct Y { pub id: i32 }"),
        ("models/inner/y.rs", "pub struct Wrong { pub id: i32 }"),
    ]);
    let items = module_items(&segs("crate::models::main::inner::y")).unwrap();
    assert_eq!(struct_names(&items), ["Y"]);
}
