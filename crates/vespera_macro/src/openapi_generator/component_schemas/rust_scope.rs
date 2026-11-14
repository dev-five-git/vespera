//! The declaring module of a `#[schema(name = "..")]`-renamed type.
//!
//! Two renames may share a Rust ident (`mod a { #[schema(name = "A")]
//! struct Foo }` + `mod b { #[schema(name = "B")] struct Foo }`); knowing
//! that one is `b::Foo` lets a module-qualified use resolve to it (see
//! `parser::schema::definition_key`).

use std::path::Path;

/// `module::Ident` of a renamed type from its derive source identity
/// (`file::Ident@line:column`): the innermost inline `mod` of the file around
/// that line, else the file's own module (`b.rs` / `b/mod.rs` → `b`).
/// `None` for an unparseable identity or a type at the crate root
/// (`lib.rs` / `main.rs`).
pub(super) fn scoped_rust_ident(source_identity: &str, rust_ident: &str) -> Option<String> {
    let (base, position) = source_identity.rsplit_once('@')?;
    let (file, _) = base.rsplit_once("::")?;
    let line = position.split_once(':')?.0.parse().ok()?;
    let file = Path::new(file);
    let module = crate::schema_macro::file_cache::get_file_content(file)
        .and_then(|content| inline_module(&content, line))
        .or_else(|| file_module(file))?;
    Some(crate::parser::schema::scoped_key(
        &module,
        rust_ident.trim_start_matches("r#"),
    ))
}

/// The module a source file declares: its stem, the directory name for a
/// `mod.rs`, nothing for the crate root.
fn file_module(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        "lib" | "main" => None,
        "mod" => file.parent()?.file_name()?.to_str().map(str::to_owned),
        _ => Some(stem.to_owned()),
    }
}

/// Name of the innermost inline `mod` whose braces enclose `line`.
///
/// Parsed with proc-macro2's fallback forced so spans carry real positions
/// (compiler spans of re-parsed text all point at the call site); the name is
/// extracted before normal mode is restored.
fn inline_module(content: &str, line: usize) -> Option<String> {
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
    innermost(&file.items, line)
}

fn innermost(items: &[syn::Item], line: usize) -> Option<String> {
    items.iter().find_map(|item| match item {
        syn::Item::Mod(syn::ItemMod {
            ident,
            content: Some((brace, inner)),
            ..
        }) if (brace.span.open().start().line..=brace.span.close().end().line).contains(&line) => {
            Some(
                innermost(inner, line)
                    .unwrap_or_else(|| syn::ext::IdentExt::unraw(ident).to_string()),
            )
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const SOURCE: &str = "struct Top;\nmod a {\n    struct Foo;\n    mod r#inner {\n        struct Foo;\n    }\n}\nstruct After;\n";

    #[rstest]
    #[case::top_level(1, None)]
    #[case::inline(3, Some("a"))]
    #[case::nested_raw(5, Some("inner"))]
    #[case::after_module(8, None)]
    fn inline_module_by_line(#[case] line: usize, #[case] expected: Option<&str>) {
        assert_eq!(inline_module(SOURCE, line).as_deref(), expected);
    }

    #[test]
    fn unparseable_source_has_no_inline_module() {
        assert_eq!(inline_module("mod a {", 1), None);
    }

    #[rstest]
    #[case::plain("src/models/b.rs", Some("b"))]
    #[case::mod_rs("src/models/b/mod.rs", Some("b"))]
    #[case::lib("src/lib.rs", None)]
    #[case::main("src/main.rs", None)]
    fn file_module_names(#[case] path: &str, #[case] expected: Option<&str>) {
        assert_eq!(file_module(Path::new(path)).as_deref(), expected);
    }

    /// The identity's line picks the inline module; a type outside any
    /// inline `mod` takes the file's module; the crate root and malformed
    /// identities give nothing.
    #[test]
    fn scoped_ident_from_source_identity() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models.rs");
        std::fs::write(&models, "mod b {\n    struct Foo;\n}\nstruct Foo;\n").unwrap();
        let lib = dir.path().join("lib.rs");
        std::fs::write(&lib, "struct Foo;\n").unwrap();
        let identity = |file: &Path, line: usize| format!("{}::Foo@{line}:11", file.display());
        assert_eq!(
            scoped_rust_ident(&identity(&models, 2), "r#Foo").as_deref(),
            Some("b::Foo")
        );
        assert_eq!(
            scoped_rust_ident(&identity(&models, 4), "Foo").as_deref(),
            Some("models::Foo")
        );
        assert_eq!(scoped_rust_ident(&identity(&lib, 1), "Foo"), None);
        for malformed in ["no-position", "Foo@1:2", "f.rs::Foo@x", "f.rs::Foo@x:1"] {
            assert_eq!(scoped_rust_ident(malformed, "Foo"), None, "{malformed}");
        }
    }
}
