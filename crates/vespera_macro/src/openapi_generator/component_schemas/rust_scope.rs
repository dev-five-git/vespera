//! The declaring module of a `#[derive(Schema)]` type, from its source identity.
//!
//! A type is registered under its full crate-relative module path so that a
//! module-qualified reference resolves to the declaration it names, not to
//! another type that merely shares the leaf module name (see
//! `parser::schema::scoped_ident`).

use std::{
    borrow::Cow,
    path::{Component, Path},
};

/// The crate-relative module path and Rust ident of the type a derive source
/// identity (`file::Ident@line:column`) names.
///
/// The module path is the file's own module chain under its crate root (the
/// nearest ancestor directory holding `lib.rs` or `main.rs` and named `src`
/// or holding `Cargo.toml`, see `is_crate_root_dir`), followed by the
/// inline `mod`s around that line. Empty for a crate-root type. `None` for an
/// unparseable identity or a file outside any crate root.
pub(super) fn crate_path(source_identity: &str) -> Option<(Vec<String>, String)> {
    let (base, position) = source_identity.rsplit_once('@')?;
    let (file, ident) = base.rsplit_once("::")?;
    let line = position.split_once(':')?.0.parse().ok()?;
    let file = Path::new(file);
    let mut modules = file_modules(file)?;
    let inline = get_inline_modules(file, line).unwrap_or_default();
    modules.extend(inline);
    Some((modules, ident.trim_start_matches("r#").to_owned()))
}

/// Whether `dir` can be a crate root: the `src` directory of a cargo package
/// or the package directory itself (the one holding `Cargo.toml`). A module
/// directory that happens to hold a module file named `main.rs` or `lib.rs`
/// (`src/api/main.rs`, declared as `mod main;`) is not one.
fn is_crate_root_dir(dir: &Path) -> bool {
    dir.file_name().is_some_and(|name| name == "src") || dir.join("Cargo.toml").is_file()
}

/// The modules a source file declares under its crate root: its path below
/// the root, with `mod.rs` naming its directory and `lib.rs` / `main.rs` at
/// the root naming nothing. The root is the nearest ancestor that is a crate
/// root by [`is_crate_root_dir`] and holds a `lib.rs` or `main.rs`.
fn file_modules(file: &Path) -> Option<Vec<String>> {
    file_modules_from(file, &std::env::current_dir().ok()?)
}

/// [`file_modules`] with the directory a relative file name is anchored to.
/// Cargo runs rustc in the package root, so a bare `models.rs` names the
/// package root's own file: anchoring it there finds that root. A `..` path
/// stays relative, since anchoring would hide the parent components.
fn file_modules_from(file: &Path, cwd: &Path) -> Option<Vec<String>> {
    let anchored;
    let file = if file.is_relative() && !file.components().any(|c| c == Component::ParentDir) {
        anchored = cwd.join(file);
        anchored.as_path()
    } else {
        file
    };
    let root = file.ancestors().skip(1).find(|dir| {
        !dir.as_os_str().is_empty()
            && (dir.join("lib.rs").is_file() || dir.join("main.rs").is_file())
            && is_crate_root_dir(dir)
    })?;
    let mut modules = Vec::new();
    for component in file.strip_prefix(root).ok()?.components() {
        if let Component::Normal(name) = component {
            modules.push(name.to_str()?.to_owned());
        }
    }
    // Cargo's binary targets (`src/bin/*.rs`, under a `src` root) and its
    // example / test / bench targets (beside `src`) are crates of their own,
    // not modules of the library: they have no module path here. A crate
    // root that declares `mod bin;` makes `src/bin/` a module of the crate.
    let root_is_src = root.file_name().is_some_and(|name| name == "src");
    let target_crate = match modules.first().map(String::as_str) {
        Some("bin") => root_is_src && !root_declares_bin(root),
        Some("examples" | "tests" | "benches") => !root_is_src,
        _ => false,
    };
    if target_crate {
        return None;
    }
    let file_name = modules.pop()?;
    match Path::new(&file_name).file_stem()?.to_str()? {
        "mod" => {}
        "lib" | "main" if modules.is_empty() => {}
        stem => modules.push(stem.to_owned()),
    }
    Some(modules)
}

/// Whether the crate root (`lib.rs`, else `main.rs`) of `root` declares a
/// top-level `mod bin;`: its `src/bin/` files are then modules of the crate.
fn root_declares_bin(root: &Path) -> bool {
    let lib = root.join("lib.rs");
    let crate_root = if lib.is_file() {
        lib
    } else {
        root.join("main.rs")
    };
    crate::schema_macro::file_cache::get_file_content(&crate_root)
        .and_then(|content| syn::parse_file(&content).ok())
        .is_some_and(|file| {
            file.items
                .iter()
                .any(|item| matches!(item, syn::Item::Mod(module) if module.ident == "bin"))
        })
}

fn get_inline_modules(file: &Path, line: usize) -> Option<Vec<String>> {
    let content = crate::schema_macro::file_cache::get_file_content(file)?;
    inline_modules(&content, line)
}

/// The inline `mod` chain a source identity sits in, outermost first (empty
/// at a file's top level). `None` for an unparseable identity.
pub(super) fn inline_chain(source_identity: &str) -> Option<Vec<String>> {
    let (base, position) = source_identity.rsplit_once('@')?;
    let (file, _) = base.rsplit_once("::")?;
    let line = position.split_once(':')?.0.parse().ok()?;
    get_inline_modules(Path::new(file), line)
}

/// The file a struct's `#[serde(default = "fn")]` functions resolve in: the
/// file itself for a top-level struct, or just the functions of the inline
/// module the struct sits in (a same-named top-level fn belongs to another
/// module). `None` when that module cannot be found in `file`.
pub(super) fn default_scope<'a>(
    file: &'a syn::File,
    source_identity: Option<&str>,
) -> Option<Cow<'a, syn::File>> {
    let chain = source_identity.and_then(inline_chain).unwrap_or_default();
    if chain.is_empty() {
        return Some(Cow::Borrowed(file));
    }
    let items = nested_items(&file.items, &chain)?;
    // An empty parse gives a `syn::File` without naming its (version-specific)
    // fields; only the functions of the module are kept.
    let mut scoped: syn::File = syn::parse_str("").ok()?;
    scoped.items = items
        .iter()
        .filter(|item| matches!(item, syn::Item::Fn(_)))
        .cloned()
        .collect();
    Some(Cow::Owned(scoped))
}

/// The items of the inline module `chain` names under `items`.
fn nested_items<'a>(items: &'a [syn::Item], chain: &[String]) -> Option<&'a [syn::Item]> {
    let Some((first, rest)) = chain.split_first() else {
        return Some(items);
    };
    let inner = items.iter().find_map(|item| match item {
        syn::Item::Mod(module) if syn::ext::IdentExt::unraw(&module.ident) == first.as_str() => {
            module.content.as_ref().map(|(_, inner)| inner.as_slice())
        }
        _ => None,
    })?;
    nested_items(inner, rest)
}

/// Names of the inline `mod`s whose braces enclose `line`, outermost first.
///
/// Parsed with proc-macro2's fallback forced so spans carry real positions
/// (compiler spans of re-parsed text all point at the call site); the names
/// are extracted before normal mode is restored.
fn inline_modules(content: &str, line: usize) -> Option<Vec<String>> {
    // A counted hold: a nested force of a parallel section stays in place.
    let _forced = crate::schema_macro::file_cache::force_fallback();
    let file = syn::parse_file(content).ok()?;
    Some(nested(&file.items, line))
}

fn nested(items: &[syn::Item], line: usize) -> Vec<String> {
    items
        .iter()
        .find_map(|item| match item {
            syn::Item::Mod(syn::ItemMod {
                ident,
                content: Some((brace, inner)),
                ..
            }) if (brace.span.open().start().line..=brace.span.close().end().line)
                .contains(&line) =>
            {
                let mut chain = vec![syn::ext::IdentExt::unraw(ident).to_string()];
                chain.extend(nested(inner, line));
                Some(chain)
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;

    use super::*;

    const SOURCE: &str = "struct Top;\nmod a {\n    struct Foo;\n    mod r#inner {\n        struct Foo;\n    }\n}\nstruct After;\n";

    #[rstest]
    #[case::top_level(1, &[])]
    #[case::inline(3, &["a"])]
    #[case::nested_raw(5, &["a", "inner"])]
    #[case::after_module(8, &[])]
    fn inline_module_chain_by_line(#[case] line: usize, #[case] expected: &[&str]) {
        assert_eq!(inline_modules(SOURCE, line).unwrap(), expected);
    }

    #[test]
    fn unparseable_source_has_no_inline_module() {
        assert_eq!(inline_modules("mod a {", 1), None);
    }

    /// A crate `src/` holding `lib.rs`: each file's chain is its path below
    /// `src/`, `mod.rs` naming its directory and `lib.rs` naming nothing.
    #[rstest]
    #[case::plain("models/b.rs", &["models", "b"])]
    #[case::mod_rs("models/b/mod.rs", &["models", "b"])]
    #[case::lib("lib.rs", &[])]
    #[case::main("main.rs", &[])]
    fn file_module_chains(#[case] path: &str, #[case] expected: &[&str]) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("models/b")).unwrap();
        fs::write(src.join("lib.rs"), "").unwrap();
        let file = src.join(path);
        fs::write(&file, "").unwrap();
        assert_eq!(file_modules(&file).unwrap(), expected);
    }

    /// A module directory holding a `main.rs` / `lib.rs` module file is not a
    /// crate root: `src/api/user.rs` is `api::user`, under `src/`.
    #[test]
    fn module_dir_holding_a_main_rs_is_not_a_crate_root() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("api")).unwrap();
        fs::write(src.join("lib.rs"), "").unwrap();
        fs::write(src.join("api").join("main.rs"), "").unwrap();
        let file = src.join("api").join("user.rs");
        fs::write(&file, "").unwrap();
        assert_eq!(file_modules(&file).unwrap(), ["api", "user"]);
    }

    /// `src/bin/tool.rs` and `examples/demo.rs` are crates of their own: no
    /// module path, so they cannot shadow a library `tool::Foo`. A library
    /// module that merely shares the name (`src/tests/helper.rs`) keeps its.
    #[test]
    fn cargo_target_files_have_no_library_module_path() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::create_dir_all(src.join("tests")).unwrap();
        fs::create_dir_all(dir.path().join("examples")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("main.rs"), "").unwrap();
        fs::write(src.join("lib.rs"), "").unwrap();
        for file in [
            src.join("bin").join("tool.rs"),
            dir.path().join("examples").join("demo.rs"),
        ] {
            fs::write(&file, "").unwrap();
            assert_eq!(file_modules(&file), None, "{}", file.display());
        }
        let helper = src.join("tests").join("helper.rs");
        fs::write(&helper, "").unwrap();
        assert_eq!(file_modules(&helper).unwrap(), ["tests", "helper"]);
    }

    /// A crate root declaring `mod bin;` makes `src/bin/` a module of the
    /// library (`bin::tool`), not a cargo binary target of its own.
    #[test]
    fn src_bin_is_a_module_when_the_root_declares_it() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(src.join("lib.rs"), "pub mod bin;").unwrap();
        let file = src.join("bin").join("tool.rs");
        fs::write(&file, "").unwrap();
        assert_eq!(file_modules(&file).unwrap(), ["bin", "tool"]);
    }

    /// A package with `lib.rs` and `models.rs` at its root (no `src/`): the
    /// bare relative name `models.rs` is the root's own file, so it is
    /// `models`. A `..` path is not anchored and never reaches the cwd root.
    #[test]
    fn bare_relative_file_at_the_package_root_has_a_module() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("lib.rs"), "").unwrap();
        assert_eq!(
            file_modules_from(Path::new("models.rs"), dir.path()).unwrap(),
            ["models"]
        );
        assert_eq!(
            file_modules_from(Path::new("lib.rs"), dir.path()).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            file_modules_from(Path::new("../models.rs"), dir.path()),
            None
        );
    }

    #[test]
    fn file_outside_a_crate_root_has_no_module() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("loose.rs");
        assert_eq!(file_modules(&file), None);
    }

    /// The identity's line adds the inline modules around it; the file adds
    /// its own chain under the crate root; the crate root and malformed
    /// identities give no path.
    #[test]
    fn crate_path_from_source_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("lib.rs"), "").unwrap();
        let models = dir.path().join("models.rs");
        fs::write(&models, "mod b {\n    struct Foo;\n}\nstruct Foo;\n").unwrap();
        let lib = dir.path().join("lib.rs");
        let identity = |file: &Path, line: usize| format!("{}::Foo@{line}:11", file.display());
        assert_eq!(
            crate_path(&identity(&models, 2)),
            Some((vec!["models".to_owned(), "b".to_owned()], "Foo".to_owned()))
        );
        assert_eq!(
            crate_path(&identity(&models, 4)),
            Some((vec!["models".to_owned()], "Foo".to_owned()))
        );
        assert_eq!(
            crate_path(&format!("{}::r#Foo@4:11", models.display())),
            Some((vec!["models".to_owned()], "Foo".to_owned()))
        );
        assert_eq!(
            crate_path(&identity(&lib, 1)),
            Some((Vec::new(), "Foo".to_owned()))
        );
        for malformed in ["no-position", "Foo@1:2", "f.rs::Foo@x", "f.rs::Foo@x:1"] {
            assert_eq!(crate_path(malformed), None, "{malformed}");
        }
    }
}
