//! Unit tests for `collector::test_scope`.

use std::fs;

use rstest::rstest;
use tempfile::TempDir;

use super::*;
use crate::metadata::{StructMetadata, TestOnlyRoute};

#[rstest]
#[case::bare_test("test", true)]
#[case::negated("not (test)", false)]
#[case::all_with_test("all (test , feature = \"x\")", true)]
#[case::any_with_other("any (test , feature = \"x\")", false)]
#[case::any_of_test_only("any (test , all (test , unix))", true)]
#[case::empty_any("any ()", false)]
#[case::feature("feature = \"x\"", false)]
#[case::platform("unix", false)]
#[case::malformed("all ((", false)]
fn test_only_predicates(#[case] predicate: &str, #[case] expected: bool) {
    assert_eq!(is_test_only_predicate(predicate), expected);
}

/// `src/` of a fresh crate with `files` (relative to `src/`).
fn crate_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (relative, content) in files {
        let path = dir.path().join("src").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    crate::schema_macro::file_cache::bump_epoch();
    dir
}

#[test]
fn files_behind_test_only_module_declarations() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "mod routes; #[cfg(test)] mod test_support; #[cfg(feature = \"x\")] mod extra;",
        ),
        (
            "routes/mod.rs",
            "pub mod users; #[cfg(all(test, unix))] mod fixtures; #[cfg(test)] mod r#async;",
        ),
        ("routes/users.rs", ""),
        ("routes/fixtures.rs", ""),
        ("routes/async.rs", ""),
        ("test_support/mod.rs", "pub mod models;"),
        ("test_support/models.rs", ""),
        ("extra.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    let gated = |scope: &mut TestScope, relative: &str| scope.file_test_only(&src.join(relative));
    assert!(!gated(&mut scope, "lib.rs"));
    assert!(!gated(&mut scope, "routes/users.rs"));
    assert!(gated(&mut scope, "routes/fixtures.rs"));
    assert!(gated(&mut scope, "routes/async.rs"));
    assert!(gated(&mut scope, "test_support/mod.rs"));
    assert!(gated(&mut scope, "test_support/models.rs"));
    // Feature predicates are not interpreted.
    assert!(!gated(&mut scope, "extra.rs"));
    // Missing files, files outside `src/` and an unknown `src/` never are.
    assert!(!gated(&mut scope, "missing.rs"));
    assert!(!scope.file_test_only(&krate.path().join("Cargo.toml")));
    assert!(!TestScope::new(None).file_test_only(&src.join("routes/fixtures.rs")));
}

/// An out-of-line `#[cfg(test)] mod` declared inside an inline module gates
/// its file (`mod a { #[cfg(test)] mod b; }` loads `src/a/b.rs`), however
/// deep the inline nesting; an ungated sibling stays production.
#[test]
fn test_only_mods_inside_inline_modules() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "mod a { #[cfg(test)] mod b; mod c; mod d { #[cfg(test)] mod e; } } #[cfg(test)] mod t { mod u; }",
        ),
        ("a/b.rs", "pub mod deep;"),
        ("a/b/deep.rs", ""),
        ("a/c.rs", ""),
        ("a/d/e.rs", ""),
        ("t/u.rs", ""),
        ("b.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    let gated = |scope: &mut TestScope, relative: &str| scope.file_test_only(&src.join(relative));
    assert!(gated(&mut scope, "a/b.rs"));
    assert!(gated(&mut scope, "a/b/deep.rs"));
    assert!(gated(&mut scope, "a/d/e.rs"));
    assert!(gated(&mut scope, "t/u.rs"));
    assert!(!gated(&mut scope, "a/c.rs"));
    // `b` is gated only as `a::b`, not at the crate root.
    assert!(!gated(&mut scope, "b.rs"));
}

/// `.` / `..` fold without touching the filesystem; relative paths are
/// absolutized; climbing above the root is no path.
#[test]
fn lexical_paths_fold_dots() {
    let dir = TempDir::new().unwrap();
    assert_eq!(
        lexical(&dir.path().join("a/./b/../c.rs")),
        Some(dir.path().join("a").join("c.rs"))
    );
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(lexical(Path::new("x/../y")), Some(cwd.join("y")));
    let root = cwd.ancestors().last().unwrap();
    assert_eq!(lexical(&root.join("..")), None);
}

/// The module chain follows the logical path: a test-gated `mod fixtures;`
/// whose directory is a symlink out of `src/` (or onto a sibling module)
/// still gates the file. A spelling of the crate that is not lexically under
/// `src/` (a symlinked alias of the crate directory) falls back to the
/// canonical path.
#[cfg(unix)]
#[test]
fn symlinked_module_directories_follow_the_logical_chain() {
    let krate = crate_with(&[
        ("lib.rs", "mod routes;"),
        (
            "routes/mod.rs",
            "#[cfg(test)] mod fixtures; #[cfg(test)] mod v2; mod v1; #[cfg(test)] mod gated;",
        ),
        ("routes/gated.rs", ""),
        ("routes/v1/mod.rs", "mod users;"),
        ("routes/v1/users.rs", ""),
    ]);
    let outside = krate.path().join("test_fixtures");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("mod.rs"), "").unwrap();
    let src = krate.path().join("src");
    std::os::unix::fs::symlink(&outside, src.join("routes/fixtures")).unwrap();
    std::os::unix::fs::symlink(src.join("routes/v1"), src.join("routes/v2")).unwrap();
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("routes/fixtures/mod.rs")));
    assert!(scope.file_test_only(&src.join("routes/v2/users.rs")));
    assert!(!scope.file_test_only(&src.join("routes/v1/users.rs")));

    let alias = TempDir::new().unwrap();
    let alias_crate = alias.path().join("krate");
    std::os::unix::fs::symlink(krate.path(), &alias_crate).unwrap();
    assert!(scope.file_test_only(&alias_crate.join("src/routes/gated.rs")));
    assert!(!scope.file_test_only(&alias_crate.join("src/routes/v1/users.rs")));
}

#[test]
fn functions_behind_test_only_cfg() {
    let krate = crate_with(&[
        (
            "gated.rs",
            "#[cfg(test)] pub async fn a() {} pub async fn b() {} #[cfg(test)] pub async fn r#type() {} #[cfg(unix)] pub async fn c() {} pub struct S;",
        ),
        ("plain.rs", "pub async fn a() {}"),
        ("broken.rs", "#[cfg(test)] fn ("),
    ]);
    let src = krate.path().join("src");
    let mut fns: Vec<String> = test_only_fns(&src.join("gated.rs")).into_iter().collect();
    fns.sort();
    assert_eq!(fns, ["a", "type"]);
    assert!(test_only_fns(&src.join("plain.rs")).is_empty());
    assert!(test_only_fns(&src.join("broken.rs")).is_empty());
    assert!(test_only_fns(&src.join("missing.rs")).is_empty());
}

fn schema(name: &str, identity: Option<String>) -> StructMetadata {
    let mut schema = StructMetadata::new(name.to_string(), format!("struct {name} {{}}"));
    schema.source_identity = identity;
    schema
}

fn route(file_path: &str, function_name: &str) -> crate::metadata::RouteMetadata {
    serde_json::from_value(serde_json::json!({
        "method": "get", "path": format!("/{function_name}"),
        "function_name": function_name, "module_path": "routes::x", "file_path": file_path,
    }))
    .unwrap()
}

/// The documented view drops test-only routes and schemas — gated items,
/// items in gated inline modules and items of gated files — and borrows
/// when there is nothing to drop.
#[test]
fn documented_view_leaves_out_test_only_items() {
    let krate = crate_with(&[
        ("lib.rs", "mod models; #[cfg(test)] mod test_models;"),
        (
            "models.rs",
            "pub struct Kept {}\n#[cfg(test)]\nmod tests {\n    pub struct Fixture {}\n    pub enum Nested { A }\n}\n#[cfg(test)] pub enum Alone { A }\nmod open { pub struct Inner {} }\nfn helper() {}\n",
        ),
        ("test_models.rs", "pub struct OnlyInTests {}"),
        ("broken.rs", "#[cfg(test)] struct ("),
        ("plain.rs", "pub struct Plain {}"),
    ]);
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", krate.path());
    let src = krate.path().join("src");
    let at = |file: &str, ident: &str, line: usize| {
        Some(format!("{}::{ident}@{line}:11", src.join(file).display()))
    };
    let mut metadata = CollectedMetadata::new();
    metadata.routes = vec![route("users.rs", "list"), route("users.rs", "debug")];
    metadata.structs = vec![
        schema("Kept", at("models.rs", "Kept", 1)),
        schema("Fixture", at("models.rs", "Fixture", 4)),
        schema("Nested", at("models.rs", "Nested", 5)),
        schema("Alone", at("models.rs", "Alone", 7)),
        schema("Inner", at("models.rs", "Inner", 8)),
        // Same name on another line: not the gated item.
        schema("Fixture", at("models.rs", "Fixture", 1)),
        schema("OnlyInTests", at("test_models.rs", "OnlyInTests", 1)),
        schema("Broken", at("broken.rs", "Broken", 1)),
        schema("Plain", at("plain.rs", "Plain", 1)),
        schema("Gone", at("gone.rs", "Gone", 1)),
        schema("Unknown", None),
        schema("Malformed", Some("no-position".to_string())),
        StructMetadata::new_model("Model".to_string(), String::new()),
    ];
    assert!(matches!(documented_view(&metadata), Cow::Owned(_)));
    metadata.test_only_routes.push(TestOnlyRoute {
        file_path: "users.rs".to_string(),
        function_name: "debug".to_string(),
    });
    let view = documented_view(&metadata);
    let routes: Vec<_> = view
        .routes
        .iter()
        .map(|r| r.function_name.as_str())
        .collect();
    assert_eq!(routes, ["list"]);
    let structs: Vec<_> = view.structs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        structs,
        [
            "Kept",
            "Inner",
            "Fixture",
            "Broken",
            "Plain",
            "Gone",
            "Unknown",
            "Malformed",
            "Model"
        ]
    );

    let mut plain = CollectedMetadata::new();
    plain.structs = vec![schema("Kept", at("models.rs", "Kept", 1))];
    plain.routes = vec![route("users.rs", "list")];
    assert!(matches!(documented_view(&plain), Cow::Borrowed(_)));
}

#[rstest]
#[case::unix("/src/models.rs::User@12:11", Some(("/src/models.rs", "User", 12)))]
#[case::windows(r"C:\src\models.rs::User@3:0", Some((r"C:\src\models.rs", "User", 3)))]
#[case::no_position("src/models.rs::User", None)]
#[case::no_ident("src/models.rs@1:0", None)]
#[case::no_column("src/models.rs::User@1", None)]
#[case::bad_line("src/models.rs::User@x:0", None)]
fn source_identities_parse(#[case] identity: &str, #[case] expected: Option<(&str, &str, usize)>) {
    assert_eq!(parse_identity(identity), expected);
}

/// Schemas nested in a test-only fn / impl / trait / const / static body or
/// emitted by a test-only item or statement macro are test-only, wherever
/// the gate sits; the same shapes without a gate are not.
#[test]
fn gated_scopes_nest_test_only_schemas() {
    let source = [
        "#[cfg(test)]",
        "mod tests {",
        "    #[test] fn t() { struct InFn {} }",
        "    schema_type!(InMod from crate::Model, pick = [\"a\"]);",
        "}",
        "#[cfg(test)] schema_type!(TopMacro from crate::Model);",
        "fn open() {",
        "    #[cfg(test)] schema_type!(StmtMacro from crate::M);",
        "    #[cfg(test)] let _x = { struct InLocal {} 1 };",
        "    let _y = { struct OpenLocal {} 1 };",
        "    struct OpenFn {}",
        "    1;",
        "}",
        "#[cfg(test)] fn gated() { struct GatedFn {} }",
        "#[cfg(test)] impl S { fn f() { struct InImpl {} } }",
        "impl S {",
        "    #[cfg(test)] fn f() { struct InImplFn {} }",
        "    #[cfg(test)] const C: () = { struct InImplConst {} };",
        "    type T = u8;",
        "    fn g() { struct OpenImplFn {} }",
        "}",
        "trait Tr {",
        "    #[cfg(test)] fn f() { struct InTraitFn {} }",
        "    const C: u8;",
        "}",
        "#[cfg(test)] trait Gt { fn f() { struct InTrait {} } }",
        "#[cfg(test)] const K: () = { struct InConst {} };",
        "#[cfg(test)] static S: () = { struct InStatic {} };",
        "use std::fmt;",
        "#[cfg(test)] use std::io;",
        "pub struct After {}",
    ]
    .join("\n");
    let krate = crate_with(&[("lib.rs", &source)]);
    let gated = gated_items(&krate.path().join("src/lib.rs"));
    for (ident, line) in [
        ("InFn", 3),
        ("InMod", 4),
        ("TopMacro", 6),
        ("StmtMacro", 8),
        ("InLocal", 9),
        ("GatedFn", 14),
        ("InImpl", 15),
        ("InImplFn", 17),
        ("InImplConst", 18),
        ("InTraitFn", 23),
        ("InTrait", 26),
        ("InConst", 27),
        ("InStatic", 28),
    ] {
        assert!(gated.contains(ident, line), "{ident}@{line} is test-only");
    }
    for (ident, line) in [
        ("OpenLocal", 10),
        ("OpenFn", 11),
        ("OpenImplFn", 20),
        ("After", 31),
    ] {
        assert!(
            !gated.contains(ident, line),
            "{ident}@{line} is not test-only"
        );
    }
}
