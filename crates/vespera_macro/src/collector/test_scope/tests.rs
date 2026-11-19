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

/// A `#[path]` module's file reached through a symlinked alias of the crate:
/// the chain check cannot match it (its module name is `support`, not the
/// file's stem), so `file_path_gated` must compare the alias spelling and the
/// lexical `#[path]` target in one spelling (round-27 macrorest-1 L1).
#[cfg(unix)]
#[test]
fn path_attribute_gated_file_through_an_alias_is_test_only() {
    let krate = crate_with(&[(
        "lib.rs",
        "#[cfg(test)] #[path = \"fixtures.rs\"] mod support; mod plain;",
    )]);
    fs::write(krate.path().join("src/fixtures.rs"), "").unwrap();
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    let alias = TempDir::new().unwrap();
    let alias_crate = alias.path().join("krate");
    std::os::unix::fs::symlink(krate.path(), &alias_crate).unwrap();
    assert!(scope.file_test_only(&alias_crate.join("src/fixtures.rs")));
    assert!(scope.file_test_only(&src.join("fixtures.rs")));
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
    let defs = fn_definitions(&src.join("gated.rs"));
    let mut fns: Vec<&str> = defs
        .iter()
        .filter(|def| def.test_only)
        .map(|def| def.name.as_str())
        .collect();
    fns.sort_unstable();
    assert_eq!(fns, ["a", "type"]);
    assert!(
        fn_definitions(&src.join("plain.rs"))
            .iter()
            .all(|def| !def.test_only)
    );
    assert!(fn_definitions(&src.join("broken.rs")).is_empty());
    assert!(fn_definitions(&src.join("missing.rs")).is_empty());
}

/// A `#[cfg(test)]` twin and a production fn share a name: the storage entry
/// of this build (the production `POST`, or the test `GET`) decides which of
/// the two is test-only, not the name.
#[test]
fn same_named_route_twins_are_told_apart_by_method_and_signature() {
    let krate = crate_with(&[(
        "twins.rs",
        "#[cfg(test)]\n#[route(get)]\npub async fn list() {}\n#[route(post)]\npub async fn list() {}\n#[cfg(test)]\n#[route(status = 201, put)]\npub async fn other() {}\n",
    )]);
    let defs = fn_definitions(&krate.path().join("src").join("twins.rs"));
    let signature = defs[1].signature.clone();
    assert!(!route_test_only(&defs, "list", "post", &signature));
    assert!(route_test_only(&defs, "list", "get", &signature));
    // A method or signature no definition has is not a test-only route.
    assert!(!route_test_only(&defs, "list", "put", &signature));
    assert!(!route_test_only(
        &defs,
        "list",
        "post",
        "async fn list (x : u8)"
    ));
    assert!(!route_test_only(&defs, "missing", "get", &signature));
    // A name whose only definitions are test-only needs no method match.
    assert!(route_test_only(&defs, "other", "put", "anything"));
}

/// The `#[vespera::route(..)]` spelling carries its method like `#[route(..)]`:
/// a `cfg(test)` GET twin next to a production POST of the same name is told
/// apart by method, not both recorded as `get`.
#[test]
fn vespera_route_twins_are_told_apart_by_method() {
    let krate = crate_with(&[(
        "vtwins.rs",
        "#[cfg(test)]\n#[vespera::route(get)]\npub async fn list() {}\n#[vespera::route(post)]\npub async fn list() {}\n",
    )]);
    let defs = fn_definitions(&krate.path().join("src").join("vtwins.rs"));
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].method, "get");
    assert_eq!(defs[1].method, "post");
    let signature = defs[1].signature.clone();
    assert!(!route_test_only(&defs, "list", "post", &signature));
    assert!(route_test_only(&defs, "list", "get", &signature));
}

/// The stored signature of an alias-expanded return type differs from the
/// source text; the one same-method definition still matches it.
#[test]
fn alias_expanded_signature_matches_its_single_method_twin() {
    let krate = crate_with(&[(
        "aliased.rs",
        "#[cfg(test)]\n#[route(get)]\npub async fn list() -> Page {}\n#[route(post)]\npub async fn list() -> Page {}\n",
    )]);
    let defs = fn_definitions(&krate.path().join("src").join("aliased.rs"));
    let expanded = "async fn list () -> Expanded";
    assert!(route_test_only(&defs, "list", "get", expanded));
    assert!(!route_test_only(&defs, "list", "post", expanded));
}

/// Two declarations load the same file, one of them ungated: the file is in
/// every build, so it is production (`file_test_only` is false).
#[test]
fn file_also_loaded_by_an_ungated_declaration_is_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "mod models;\n#[cfg(test)]\n#[path = \"models.rs\"]\nmod test_models;\n",
        ),
        ("models.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("models.rs")));
}

/// A gated `mod` whose name an ungated `mod` of the same file also declares
/// (`#[cfg(test)] mod shared;` next to `#[cfg(not(test))] mod shared;`) loads
/// the file in every build, so the file is production.
#[test]
fn file_also_loaded_by_an_ungated_chain_declaration_is_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\nmod shared;\n#[cfg(not(test))]\nmod shared;\n",
        ),
        ("shared.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("shared.rs")));
}

/// An ungated `#[path]` module loads its file in every build, even beside a
/// gated `#[path]` module of the same file.
/// A file an ungated `#[path]` module loads stays production when a
/// test-gated `mod` of the same name also exists (round-29 macrorest-1 M1).
#[test]
fn ungated_path_module_keeps_a_file_beside_a_gated_twin_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)] mod helpers; #[path = \"helpers.rs\"] mod helpers_prod;",
        ),
        ("helpers.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("helpers.rs")));
}

#[test]
fn file_also_loaded_by_an_ungated_path_is_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"shared.rs\"]\nmod test_shared;\n#[path = \"shared.rs\"]\nmod shared;\n",
        ),
        ("shared.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("shared.rs")));
}

/// Only the target of an ungated `#[path]` module is loaded in every build;
/// its own `#[cfg(test)]` child stays test-only (round-30 L1, a regression of
/// round 29, which classified every file under the target's directory as
/// loaded).
#[test]
fn test_gated_child_of_an_ungated_path_module_is_test_only() {
    let krate = crate_with(&[
        ("lib.rs", "#[path = \"support.rs\"]\nmod support;\n"),
        (
            "support.rs",
            "#[cfg(test)]\nmod fixtures;\npub struct Kept {}\n",
        ),
        ("support/fixtures.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("support/fixtures.rs")));
    assert!(!scope.file_test_only(&src.join("support.rs")));
}

/// The same for a non-mod-rs target: the children of `beside/support.rs` sit
/// beside it (`beside/child.rs`, as rustc resolves them), so a test-gated
/// child is test-only, and a sibling of the target that is no child is not.
#[test]
fn test_gated_child_of_an_ungated_path_module_beside_its_file_is_test_only() {
    let krate = crate_with(&[
        ("lib.rs", "#[path = \"beside/support.rs\"]\nmod support;\n"),
        ("beside/support.rs", "#[cfg(test)]\nmod child;\n"),
        ("beside/child.rs", "pub struct Fixture {}"),
        ("beside/other.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("beside/child.rs")));
    assert!(!scope.file_test_only(&src.join("beside/other.rs")));
}

/// A test-gated `#[path]` module whose target is not `mod.rs` gates the
/// children declared in that target, which sit beside it (`beside/child.rs`),
/// not in a directory named after it.
#[test]
fn path_attributed_test_module_gates_children_beside_a_non_mod_rs_file() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"beside/support.rs\"]\nmod support;\n",
        ),
        ("beside/support.rs", "mod child;"),
        ("beside/child.rs", "pub struct Fixture {}"),
        ("beside/support/child.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("beside/child.rs")));
    assert!(!scope.file_test_only(&src.join("beside/support/child.rs")));
}

/// An ungated `#[path]` declared in a non-root file without any `cfg`/`test`
/// text still loads its file in every build (round-30 L2): `api/mod.rs`
/// loads `shared.rs`, which `lib.rs` gates by name.
#[test]
fn ungated_path_declaration_in_a_non_root_file_keeps_the_file_production() {
    let krate = crate_with(&[
        ("lib.rs", "#[cfg(test)]\nmod shared;\npub mod api;\n"),
        (
            "api/mod.rs",
            "#[path = \"../shared.rs\"]\npub mod shared_prod;\n",
        ),
        ("shared.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("shared.rs")));
}

/// The inline block of an ungated `#[path]` target nests its children
/// beside the target too (`beside/inner/deep.rs`, as rustc resolves a path
/// inside an inline module of a path-loaded file); a test-gated one is
/// test-only.
#[test]
fn test_gated_path_inside_an_inline_block_of_an_ungated_path_target() {
    let krate = crate_with(&[
        ("lib.rs", "#[path = \"beside/support.rs\"]\nmod support;\n"),
        (
            "beside/support.rs",
            "mod inner {\n    #[cfg(test)]\n    #[path = \"deep.rs\"]\n    mod probe;\n}\n",
        ),
        ("beside/inner/deep.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("beside/inner/deep.rs")));
}

/// An ungated `#[path]` declared in a file only test builds load is loaded
/// only by them (round-30 L2 records it, so the declarer's own gate must
/// count): `lib.rs` gates `fixtures`, and `fixtures.rs` names `shared.rs`.
#[test]
fn ungated_path_declared_in_a_test_only_file_stays_test_only() {
    let krate = crate_with(&[
        ("lib.rs", "#[cfg(test)]\nmod fixtures;\n"),
        (
            "fixtures.rs",
            "#[path = \"shared.rs\"]\npub mod shared_prod;\n",
        ),
        ("shared.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("shared.rs")));
}

/// An ungated `mod` of a production file loads a file the crate otherwise
/// gates by `#[path]` (`routes/mod.rs` declares `pub mod probe;` ungated, next
/// to `#[cfg(test)] #[path = "routes/probe.rs"] mod probe_mod;` in `lib.rs`):
/// every build loads `probe.rs`, so it is production.
#[test]
fn ungated_mod_of_a_production_file_loads_a_path_gated_file() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"routes/probe.rs\"]\nmod probe_mod;\npub mod routes;\n",
        ),
        ("routes/mod.rs", "pub mod probe;\n"),
        ("routes/probe.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("routes/probe.rs")));
}

/// Two `#[path]`s naming each other (`a.rs` declares `b.rs`, which declares
/// `a.rs` again) terminate. A cycle has no build's module tree, so it counts
/// as loaded: the files stay production, the conservative answer for a
/// document.
#[test]
fn path_cycle_terminates_and_stays_production() {
    let krate = crate_with(&[
        ("lib.rs", "#[cfg(test)]\n#[path = \"a.rs\"]\nmod a;\n"),
        ("a.rs", "#[path = \"b.rs\"]\npub mod b;\n"),
        ("b.rs", "#[path = \"a.rs\"]\npub mod a_again;\n"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("b.rs")));
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
        // The identity names the ident's own column, as the derive records it
        // (a same-name identity on a line without it keeps a placeholder).
        let text = std::fs::read_to_string(src.join(file)).unwrap_or_default();
        let column = text
            .lines()
            .nth(line - 1)
            .and_then(|text| text.find(ident))
            .unwrap_or(11);
        Some(format!(
            "{}::{ident}@{line}:{column}",
            src.join(file).display()
        ))
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
        method: "get".to_string(),
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
#[case::unix("/src/models.rs::User@12:11", Some(("/src/models.rs", "User", (12, 11))))]
#[case::windows(r"C:\src\models.rs::User@3:0", Some((r"C:\src\models.rs", "User", (3, 0))))]
#[case::no_position("src/models.rs::User", None)]
#[case::no_ident("src/models.rs@1:0", None)]
#[case::no_column("src/models.rs::User@1", None)]
#[case::bad_line("src/models.rs::User@x:0", None)]
#[case::bad_column("src/models.rs::User@1:y", None)]
fn source_identities_parse(
    #[case] identity: &str,
    #[case] expected: Option<(&str, &str, (usize, usize))>,
) {
    assert_eq!(parse_identity(identity), expected);
}

/// Schemas nested in a test-only fn / impl / trait / const / static body, a
/// `#[test]` / `#[tokio::test]` fn (outside any `cfg(test)` module too) or
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
        "#[test] fn top() { struct InTestFn {} }",
        "#[tokio::test] async fn top_async() { struct InTokioTest {} }",
        "impl S { #[test] fn h() { struct InImplTest {} } }",
        "#[inline] fn attested() { struct OpenAttr {} }",
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
        ("InTestFn", 32),
        ("InTokioTest", 33),
        ("InImplTest", 34),
    ] {
        let at = position_of(&source, ident);
        assert_eq!(at.0, line, "{ident} is on line {line}");
        assert!(gated.contains(ident, at), "{ident}@{line} is test-only");
    }
    for (ident, line) in [
        ("OpenLocal", 10),
        ("OpenFn", 11),
        ("OpenImplFn", 20),
        ("After", 31),
        ("OpenAttr", 35),
    ] {
        let at = position_of(&source, ident);
        assert_eq!(at.0, line, "{ident} is on line {line}");
        assert!(
            !gated.contains(ident, at),
            "{ident}@{line} is not test-only"
        );
    }
}

/// `(line, column)` of the first whole-word `ident` in `source`, as
/// `proc_macro2` reports a span start (0-based column, in characters).
fn position_of(source: &str, ident: &str) -> (usize, usize) {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let at = source
        .match_indices(ident)
        .map(|(at, _)| at)
        .find(|&at| {
            source[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident(c))
                && source[at + ident.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_ident(c))
        })
        .expect("ident occurs in the source");
    let line_start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    (
        source[..at].matches('\n').count() + 1,
        source[line_start..at].chars().count(),
    )
}

/// A file whose only gate is a `#[test]` fn (no `cfg` anywhere) is still
/// scanned: `cargo test` registers the schema, `cargo build` never does.
#[test]
fn bare_test_fn_without_cfg_gates_its_schemas() {
    let source = "#[test]\nfn probe() { struct Probe {} }\npub struct Kept {}";
    let krate = crate_with(&[("lib.rs", source)]);
    let gated = gated_items(&krate.path().join("src/lib.rs"));
    assert!(gated.contains("Probe", position_of(source, "Probe")));
    assert!(!gated.contains("Kept", position_of(source, "Kept")));
}

/// A test-only range is matched by position: a struct on the same line as a
/// gated module, after it, is not test-only (and one inside it is).
#[test]
fn gated_ranges_match_by_column_not_just_line() {
    let source = "#[cfg(test)] mod t { pub struct Fixture {} } pub struct Public { pub id: i32 }";
    let krate = crate_with(&[("lib.rs", source)]);
    let gated = gated_items(&krate.path().join("src/lib.rs"));
    assert!(gated.contains("Fixture", position_of(source, "Fixture")));
    assert!(!gated.contains("Public", position_of(source, "Public")));
}

/// A `#[cfg(test)] #[path = ".."] mod` gates the file its path names, not a
/// file named after the module (`support.rs` here stays production).
#[test]
fn path_attributed_test_module_gates_the_file_it_names() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"fixtures.rs\"]\nmod support;\n",
        ),
        ("fixtures.rs", "pub struct Fixture {}"),
        ("support.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("fixtures.rs")));
    assert!(!scope.file_test_only(&src.join("support.rs")));
    assert!(!scope.file_test_only(&src.join("lib.rs")));
}

/// The child modules of a test-gated `#[path = "fixtures/mod.rs"]` module
/// live beside that `mod.rs`, so they are test-only too.
#[test]
fn path_attributed_test_mod_rs_gates_its_child_modules() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = \"fixtures/mod.rs\"]\nmod fixtures;\n",
        ),
        ("fixtures/mod.rs", "pub mod helpers;"),
        ("fixtures/helpers.rs", "pub struct Fixture {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("fixtures/mod.rs")));
    assert!(scope.file_test_only(&src.join("fixtures/helpers.rs")));
}

/// A `#[cfg(test)] #[path]` module inside an inline module of a non-mod-rs
/// file (`routes.rs`) resolves under `routes/<inline>/`, as rustc does, not
/// under `<inline>/` beside the declarer.
#[test]
fn path_attributed_test_module_inside_inline_mod_of_non_mod_rs_file() {
    let krate = crate_with(&[
        ("lib.rs", "mod routes;\n"),
        (
            "routes.rs",
            "mod v2 {\n    #[cfg(test)]\n    #[path = \"fixtures.rs\"]\n    mod support;\n}\n",
        ),
        ("routes/v2/fixtures.rs", "pub struct Fixture {}"),
        ("v2/fixtures.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("routes/v2/fixtures.rs")));
    assert!(!scope.file_test_only(&src.join("v2/fixtures.rs")));
}

/// A `#[path]` module nested in a test-gated inline module names a file only
/// test builds load, even though the inline module itself is the chain the
/// gate is found by. Without the recursion into the gated module the file is
/// classified as production. An ungated inline module's `#[path]` stays open.
#[test]
fn path_module_nested_in_test_only_inline_mod_gates_its_file() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[path = \"../routes/probe.rs\"]\n    mod probe;\n}\nmod shared {\n    #[path = \"../routes/kept.rs\"]\n    mod kept;\n}\n",
        ),
        ("routes/probe.rs", "pub async fn probe() {}"),
        ("routes/kept.rs", "pub async fn kept() {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("routes/probe.rs")));
    assert!(!scope.file_test_only(&src.join("routes/kept.rs")));
}
/// A `#[path]` whose value is not a string literal names no file, so the
/// module falls back to the file named after it.
#[test]
fn non_literal_path_attribute_falls_back_to_the_module_file() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(test)]\n#[path = concat!(\"fix\", \"tures.rs\")]\nmod support;\n",
        ),
        ("fixtures.rs", "pub struct Fixture {}"),
        ("support.rs", "pub struct Kept {}"),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("fixtures.rs")));
    assert!(scope.file_test_only(&src.join("support.rs")));
}

/// A test-only `GET` twin leaves a production `POST` route of the same name
/// in the document: the entry records the method it was registered with.
#[test]
fn documented_view_keeps_production_twin_of_another_method() {
    let mut metadata = CollectedMetadata::new();
    metadata.routes = vec![route("users.rs", "list"), route("users.rs", "list")];
    metadata.routes[1].method = "post".to_string();
    metadata.test_only_routes = vec![TestOnlyRoute {
        file_path: "users.rs".to_string(),
        function_name: "list".to_string(),
        method: "get".to_string(),
    }];
    let view = documented_view(&metadata);
    let methods: Vec<&str> = view.routes.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["post"]);
}

/// A declarer with no `cfg`/`test` text still declares its modules in every
/// build: `main.rs` (a bin next to a lib that gates `fixtures`) loads
/// `fixtures.rs` and `shared.rs` ungated, so neither is test-only.
#[test]
fn ungated_declaration_in_a_file_without_cfg_text_keeps_the_file_production() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "pub mod routes; #[cfg(test)] mod fixtures; #[cfg(test)] #[path = \"shared.rs\"] mod t;",
        ),
        (
            "main.rs",
            "mod fixtures; #[path = \"shared.rs\"] mod shared_m; fn main() {}",
        ),
        ("fixtures.rs", ""),
        ("shared.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("fixtures.rs")));
    assert!(!scope.file_test_only(&src.join("shared.rs")));
}

/// A deeper ungated declaration cannot cancel a shallower test-only `mod`
/// (round-28 macrorest-1 M1): `fixtures` is gated in `routes/mod.rs`, and
/// `fixtures/mod.rs` declares `models` ungated, so `models.rs` is loaded only
/// by `cfg(test)` builds and is test-only.
#[test]
fn deeper_ungated_declaration_does_not_cancel_a_gated_parent() {
    let krate = crate_with(&[
        ("lib.rs", "mod routes;"),
        ("routes/mod.rs", "#[cfg(test)] mod fixtures;"),
        (
            "routes/fixtures/mod.rs",
            "#[cfg(unix)] pub mod models; // test helpers",
        ),
        ("routes/fixtures/models.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(scope.file_test_only(&src.join("routes/fixtures/models.rs")));
}

/// The gated link declared ungated beside its gated twin (round-28
/// macrorest-1 M1, the other side): `routes/mod.rs` loads `fixtures` in every
/// build, so the file behind it is production.
#[test]
fn ungated_twin_of_a_gated_parent_keeps_the_file_production() {
    let krate = crate_with(&[
        ("lib.rs", "mod routes;"),
        ("routes/mod.rs", "#[cfg(test)] mod fixtures; mod fixtures;"),
        ("routes/fixtures/mod.rs", "pub mod models;"),
        ("routes/fixtures/models.rs", ""),
    ]);
    let src = krate.path().join("src");
    let mut scope = TestScope::new(Some(&src));
    assert!(!scope.file_test_only(&src.join("routes/fixtures/models.rs")));
}

/// `#[cfg_attr(test, derive(Schema))]` derives the schema only in `cfg(test)`
/// builds (round-28 macrorest-1 L2), so the struct is test-only; a plain
/// `derive(Schema)` beside it, or another predicate, keeps it production.
#[test]
fn cfg_attr_schema_derive_under_test_is_test_only() {
    let source = "#[cfg_attr(test, derive(Schema))] struct Probe {}\n\
                  #[derive(Schema)] struct Plain {}\n\
                  #[cfg_attr(test, derive(Schema))] #[derive(Schema)] struct Both {}\n\
                  #[cfg_attr(feature = \"x\", derive(Schema))] struct Feature {}\n";
    let krate = crate_with(&[("lib.rs", source)]);
    let gated = gated_items(&krate.path().join("src/lib.rs"));
    assert!(gated.contains("Probe", position_of(source, "Probe")));
    for ident in ["Plain", "Both", "Feature"] {
        assert!(
            !gated.contains(ident, position_of(source, ident)),
            "{ident}"
        );
    }
}

#[rstest]
#[case::cfg_attr_test("#[cfg_attr(test, derive(Schema))] struct S;", true)]
#[case::cfg_attr_list(
    "#[cfg_attr(all(test, unix), derive(serde::Serialize, vespera::Schema))] struct S;",
    true
)]
#[case::plain_derive("#[derive(Schema)] struct S;", false)]
#[case::unconditional_beside_gated(
    "#[cfg_attr(test, derive(Schema))] #[derive(Schema)] struct S;",
    false
)]
#[case::other_predicate("#[cfg_attr(feature = \"x\", derive(Schema))] struct S;", false)]
#[case::test_beside_docs_derive(
    "#[cfg_attr(test, derive(Schema))] #[cfg_attr(feature = \"docs\", derive(Schema))] struct S;",
    false
)]
#[case::test_beside_not_test_derive(
    "#[cfg_attr(test, derive(Schema))] #[cfg_attr(not(test), derive(Schema))] struct S;",
    false
)]
#[case::other_derive("#[cfg_attr(test, derive(Debug))] struct S;", false)]
#[case::other_attribute("#[cfg_attr(test, allow(dead_code))] struct S;", false)]
#[case::bare_attribute("#[inline] #[cfg_attr(test, derive(Schema))] struct S;", true)]
#[case::unparseable_cfg_attr("#[cfg_attr(= 1)] struct S;", false)]
#[case::other_list_attribute(
    "#[serde(rename_all = \"camelCase\")] #[cfg_attr(test, derive(Schema))] struct S;",
    true
)]
fn schema_derive_under_a_test_only_cfg_attr(#[case] source: &str, #[case] expected: bool) {
    let item: syn::ItemStruct = syn::parse_str(source).unwrap();
    assert_eq!(schema_derive_test_only(&item.attrs), expected);
}
