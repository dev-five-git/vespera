//! Lookup keys for types two `#[schema(name = "..")]` renames share.
//!
//! `mod a { #[schema(name = "A")] struct Foo }` and `mod b { #[schema(name =
//! "B")] struct Foo }` both alias the Rust ident `Foo`; a bare `Foo` cannot
//! tell them apart (rejected when used, see
//! `openapi_generator::component_schemas::check_ambiguous_aliases`). Each
//! rename is ALSO registered under `<declaring module>::Foo` (`a::Foo`,
//! `b::Foo`), so a module-qualified use (`b::Foo`, `crate::models::b::Foo`)
//! resolves by its last module segment to the type it really names.

use syn::ext::IdentExt;

/// `module::Ident` — the scoped lookup key of a renamed type `ident`
/// declared in `module` (its innermost module's name).
#[must_use]
pub fn scoped_key(module: &str, ident: &str) -> String {
    format!("{module}::{ident}")
}

/// Lookup key of the type `path` names: its scoped `module::Ident` key when
/// `path` is module-qualified and `registered` knows that key, else the bare
/// last-segment ident. `None` for a segment-less path.
pub fn definition_key(path: &syn::Path, registered: impl Fn(&str) -> bool) -> Option<String> {
    let mut segments = path.segments.iter().rev();
    let ident = segments.next()?.ident.unraw().to_string();
    let scoped = segments
        .next()
        .map(|module| scoped_key(&module.ident.unraw().to_string(), &ident))
        .filter(|scoped| registered(scoped));
    Some(scoped.unwrap_or(ident))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::bare("Foo", Some("Foo"))]
    #[case::scoped("b::Foo", Some("b::Foo"))]
    #[case::long_scoped("crate::models::b::Foo", Some("b::Foo"))]
    #[case::raw_module("r#b::Foo<i32>", Some("b::Foo"))]
    #[case::unregistered_scope("c::Foo", Some("Foo"))]
    fn keys(#[case] path: &str, #[case] expected: Option<&str>) {
        let path: syn::Path = syn::parse_str(path).unwrap();
        let key = definition_key(&path, |key| ["a::Foo", "b::Foo"].contains(&key));
        assert_eq!(key.as_deref(), expected);
    }

    /// M9: a module-qualified use of a shared renamed ident references the
    /// component of the type its module declares; an unregistered module
    /// (and the bare ident) keep the ident's alias.
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
            ("b::Foo", b),
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
