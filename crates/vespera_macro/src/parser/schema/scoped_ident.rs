//! Lookup keys for types whose Rust ident is shared across modules.
//!
//! `mod a { #[schema(name = "A")] struct Foo }` and `mod b { #[schema(name =
//! "B")] struct Foo }` both alias the Rust ident `Foo`; a bare `Foo` cannot
//! tell them apart (rejected when used, see
//! `openapi_generator::component_schemas::check_ambiguous_aliases`).
//!
//! Every struct with a known declaring module is registered under its full
//! crate-relative path, `crate::a::b::Foo`, and under each module-suffix of
//! that path (`b::Foo`, `a::b::Foo`). A reference is looked up the same way:
//! a `crate::`-anchored path matches only the exact key, a relative one
//! (`b::Foo` after `use crate::a::b;`) matches a suffix. A key that more than
//! one declaration claims is never registered, so the reference falls back to
//! the bare ident instead of picking one of the declarations.

use syn::ext::IdentExt;

/// The keys a type `ident` declared at crate-relative module path `modules`
/// is registered under: the exact `crate::m1::..::ident` first, then every
/// suffix of `m1::..::ident`. Empty for a crate-root type (no module).
#[must_use]
pub fn registry_keys(modules: &[impl AsRef<str>], ident: &str) -> Vec<String> {
    if modules.is_empty() {
        return Vec::new();
    }
    let mut keys = vec![format!(
        "crate::{}::{ident}",
        join(modules.iter().map(AsRef::as_ref))
    )];
    keys.extend((0..modules.len()).map(|start| {
        format!(
            "{}::{ident}",
            join(modules[start..].iter().map(AsRef::as_ref))
        )
    }));
    keys
}

/// The registry key `path` names: `crate::`-anchored paths keep their full
/// module path (`crate::a::b::Foo` → `crate::a::b::Foo`), relative ones their
/// module segments as written (`b::Foo`). `self` / `super` segments are
/// dropped (their meaning needs the referencing module, so the rest is matched
/// as a suffix). `None` for a path with no module segment.
#[must_use]
pub fn lookup_key(path: &syn::Path) -> Option<String> {
    let mut segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.unraw().to_string())
        .collect();
    let ident = segments.pop()?;
    let anchored = segments.first().is_some_and(|first| first == "crate");
    if anchored {
        segments.remove(0);
    }
    segments.retain(|segment| segment != "self" && segment != "super");
    if segments.is_empty() {
        return None;
    }
    Some(if anchored {
        format!("crate::{}::{ident}", segments.join("::"))
    } else {
        format!("{}::{ident}", segments.join("::"))
    })
}

/// Lookup key of the type `path` names: its [`lookup_key`] when `registered`
/// knows that key, else the bare last-segment ident. `None` for a segment-less
/// path.
pub fn definition_key(path: &syn::Path, registered: impl Fn(&str) -> bool) -> Option<String> {
    let ident = path.segments.last()?.ident.unraw().to_string();
    let scoped = lookup_key(path).filter(|key| registered(key));
    Some(scoped.unwrap_or(ident))
}

fn join<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts.collect::<Vec<_>>().join("::")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn path(text: &str) -> syn::Path {
        syn::parse_str(text).unwrap()
    }

    #[rstest]
    #[case::bare("Foo", None)]
    #[case::relative("b::Foo", Some("b::Foo"))]
    #[case::anchored("crate::models::b::Foo", Some("crate::models::b::Foo"))]
    #[case::raw_module("r#b::Foo<i32>", Some("b::Foo"))]
    #[case::crate_root_item("crate::Foo", None)]
    #[case::self_module("self::b::Foo", Some("b::Foo"))]
    #[case::super_module("super::b::Foo", Some("b::Foo"))]
    fn lookup_keys(#[case] text: &str, #[case] expected: Option<&str>) {
        assert_eq!(lookup_key(&path(text)).as_deref(), expected);
    }

    #[test]
    fn registry_keys_cover_the_exact_path_and_its_suffixes() {
        assert_eq!(
            registry_keys(&["models", "user"], "User"),
            [
                "crate::models::user::User",
                "models::user::User",
                "user::User"
            ]
        );
        assert_eq!(registry_keys(&["b"], "Foo"), ["crate::b::Foo", "b::Foo"]);
        assert_eq!(registry_keys(&[] as &[&str], "Foo"), Vec::<String>::new());
    }

    /// A registered key is used; anything else falls back to the bare ident.
    #[rstest]
    #[case::relative_registered("b::Foo", Some("b::Foo"))]
    #[case::anchored_registered("crate::b::Foo", Some("crate::b::Foo"))]
    #[case::anchored_other_module("crate::models::b::Foo", Some("Foo"))]
    #[case::unregistered_scope("c::Foo", Some("Foo"))]
    #[case::bare("Foo", Some("Foo"))]
    fn definition_keys(#[case] text: &str, #[case] expected: Option<&str>) {
        let key = definition_key(&path(text), |key| {
            ["crate::b::Foo", "b::Foo"].contains(&key)
        });
        assert_eq!(key.as_deref(), expected);
    }

    /// M9: a renamed type's scoped key resolves a module-qualified use to the
    /// rename its module declares; an unregistered module (and the bare ident)
    /// keep the ident's alias.
    #[rstest]
    #[case::scoped_a("a::Foo", "#/components/schemas/A")]
    #[case::scoped_b("crate::b::Foo", "#/components/schemas/B")]
    #[case::unregistered_scope("c::Foo", "#/components/schemas/A")]
    #[case::bare("Foo", "#/components/schemas/A")]
    fn qualified_type_references_its_module_s_rename(#[case] ty: &str, #[case] expected: &str) {
        let a = r#"#[schema(name = "A")] struct Foo { a: i32 }"#;
        let b = r#"#[schema(name = "B")] struct Foo { b: i32 }"#;
        let definitions = std::collections::HashMap::from([
            ("A", a),
            ("B", b),
            ("Foo", a),
            ("a::Foo", a),
            ("crate::a::Foo", a),
            ("b::Foo", b),
            ("crate::b::Foo", b),
        ]);
        let known: std::collections::HashSet<&str> = definitions.keys().copied().collect();
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let schema = crate::parser::schema::parse_type_to_schema_ref(&ty, &known, &definitions);
        let vespera_core::schema::SchemaRef::Ref(reference) = schema else {
            panic!("expected a $ref, got {schema:?}");
        };
        assert_eq!(reference.ref_path, expected);
    }

    #[test]
    fn segment_less_path_has_no_key() {
        let path = syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        };
        assert_eq!(definition_key(&path, |_| true), None);
    }
}
