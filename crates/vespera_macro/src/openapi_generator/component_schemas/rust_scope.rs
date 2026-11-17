//! The declaring module of a `#[derive(Schema)]` type, from its source identity.
//!
//! A type is registered under its full crate-relative module path so that a
//! module-qualified reference resolves to the declaration it names, not to
//! another type that merely shares the leaf module name (see
//! `parser::schema::scoped_ident`).

use std::path::{Component, Path};

/// The crate-relative module path and Rust ident of the type a derive source
/// identity (`file::Ident@line:column`) names.
///
/// The module path is the file's own module chain under its crate root (the
/// nearest ancestor directory holding `lib.rs` or `main.rs`), followed by the
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
/// the root naming nothing.
fn file_modules(file: &Path) -> Option<Vec<String>> {
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
    let file_name = modules.pop()?;
    match Path::new(&file_name).file_stem()?.to_str()? {
        "mod" => {}
        "lib" | "main" if modules.is_empty() => {}
        stem => modules.push(stem.to_owned()),
    }
    Some(modules)
}

fn get_inline_modules(file: &Path, line: usize) -> Option<Vec<String>> {
    let content = crate::schema_macro::file_cache::get_file_content(file)?;
    inline_modules(&content, line)
}

/// Names of the inline `mod`s whose braces enclose `line`, outermost first.
///
/// Parsed with proc-macro2's fallback forced so spans carry real positions
/// (compiler spans of re-parsed text all point at the call site); the names
/// are extracted before normal mode is restored.
fn inline_modules(content: &str, line: usize) -> Option<Vec<String>> {
    /// Restores proc-macro2's normal mode even if parsing panics.
    struct Unforce;
    impl Drop for Unforce {
        fn drop(&mut self) {
            proc_macro2::fallback::unforce();
        }
    }
    proc_macro2::fallback::force();
    let _unforce = Unforce;
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
