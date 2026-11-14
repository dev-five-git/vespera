//! `serde_repr` detection for `#[derive(Schema)]` enums.
//!
//! rustc hands a derive macro its item WITHOUT the `#[derive(..)]` attribute
//! that invoked it, so in `#[derive(Serialize_repr, Deserialize_repr, Schema)]`
//! the `serde_repr` derives are invisible to `Schema`. They decide the wire
//! shape (the integer discriminant instead of the variant name), so the
//! enum's derive attributes are looked up in its source file and the
//! `serde_repr` ones are copied into the stored definition, where the schema
//! parser reads them (`enum_schema::unit::derives_serde_repr`).

use std::path::Path;

const SERDE_REPR_DERIVES: [&str; 2] = ["Serialize_repr", "Deserialize_repr"];

/// `input` with the `serde_repr` derives its source declaration carries
/// (`#[derive(Serialize_repr)]` / `#[derive(Deserialize_repr)]`) appended
/// to its attributes, or `None` when there is nothing to add.
pub(super) fn with_serde_repr_derives(
    input: &syn::DeriveInput,
    call_site_file: Option<&Path>,
) -> Option<syn::DeriveInput> {
    if !matches!(input.data, syn::Data::Enum(_)) {
        return None;
    }
    let path = call_site_file?;
    let file = crate::schema_macro::file_cache::get_file_content(path)?;
    if !file.contains("_repr") {
        return None;
    }
    let source_derives = source_repr_derives(&file, &input.ident)?;
    let missing: Vec<&str> = source_derives
        .into_iter()
        .filter(|name| !derives(&input.attrs, name))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut input = input.clone();
    for name in missing {
        let ident = syn::Ident::new(name, proc_macro2::Span::call_site());
        input.attrs.push(syn::parse_quote!(#[derive(#ident)]));
    }
    Some(input)
}

/// The `serde_repr` derives of THE source declaration of the enum `ident`.
///
/// Same-named enums may live in different inline `mod`s (or fn bodies) of one
/// file, so the declaration is matched by the ident's `line:column`; when no
/// declaration sits there (the item moved under a long-lived proc-macro
/// server), a single same-named enum is still unambiguous.  Several candidates
/// and no positional match → `None` (never borrow another enum's derives).
///
/// The file is parsed with proc-macro2's fallback forced so its spans carry
/// real positions (compiler spans of re-parsed text all point at the call
/// site), and every result is computed before normal mode is restored.
fn source_repr_derives(content: &str, ident: &syn::Ident) -> Option<Vec<&'static str>> {
    /// Restores proc-macro2's normal mode even if parsing panics.
    struct Unforce;
    impl Drop for Unforce {
        fn drop(&mut self) {
            proc_macro2::fallback::unforce();
        }
    }
    struct Finder {
        ident: String,
        found: Vec<((usize, usize), Vec<&'static str>)>,
    }
    impl<'ast> syn::visit::Visit<'ast> for Finder {
        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            if item.ident == self.ident {
                let start = item.ident.span().start();
                let repr = SERDE_REPR_DERIVES
                    .into_iter()
                    .filter(|name| derives(&item.attrs, name))
                    .collect();
                self.found.push(((start.line, start.column), repr));
            }
            syn::visit::visit_item_enum(self, item);
        }
    }
    let start = ident.span().start();
    let mut finder = Finder {
        ident: ident.to_string(),
        found: Vec::new(),
    };
    proc_macro2::fallback::force();
    let _unforce = Unforce;
    let file = syn::parse_file(content).ok()?;
    syn::visit::Visit::visit_file(&mut finder, &file);
    let index = finder
        .found
        .iter()
        .position(|(position, _)| *position == (start.line, start.column))
        .or_else(|| (finder.found.len() == 1).then_some(0))?;
    Some(finder.found.swap_remove(index).1)
}

/// Whether a `#[derive(..)]` among `attrs` names `name` (by any path).
fn derives(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                found |= meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == name);
                Ok(())
            });
            found
        })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::both(
        "#[derive(Serialize_repr, Deserialize_repr, Schema)] #[repr(u8)] enum E { A }",
        &["Serialize_repr", "Deserialize_repr"]
    )]
    #[case::qualified_serialize_only(
        "#[derive(serde_repr::Serialize_repr, Schema)] #[repr(u8)] enum E { A }",
        &["Serialize_repr"]
    )]
    #[case::in_inline_module(
        "struct S; mod m { #[derive(Deserialize_repr, Schema)] #[repr(u8)] enum E { A } }",
        &["Deserialize_repr"]
    )]
    #[case::plain_serde(
        "#[derive(Serialize, Schema)] #[repr(u8)] enum E { A }",
        &[]
    )]
    fn copies_serde_repr_derives_from_source(#[case] source: &str, #[case] expected: &[&str]) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, source).unwrap();
        let input: syn::DeriveInput = syn::parse_str("#[repr(u8)] enum E { A }").unwrap();
        let found: Vec<&str> = with_serde_repr_derives(&input, Some(&path))
            .map(|input| {
                SERDE_REPR_DERIVES
                    .into_iter()
                    .filter(|name| derives(&input.attrs, name))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(found, expected);
    }

    /// A derive the input already shows (listed in a later `#[derive]`)
    /// is not added twice.
    #[test]
    fn visible_serde_repr_derive_is_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(
            &path,
            "#[derive(Schema)] #[derive(Serialize_repr)] #[repr(u8)] enum E { A }",
        )
        .unwrap();
        let input: syn::DeriveInput =
            syn::parse_str("#[derive(Serialize_repr)] #[repr(u8)] enum E { A }").unwrap();
        assert!(with_serde_repr_derives(&input, Some(&path)).is_none());
    }

    /// The enum `nth` (0-based) named `E` in `source`, as a derive input whose
    /// ident keeps its source position.
    fn enum_at(source: &str, nth: usize) -> syn::DeriveInput {
        struct Collect(Vec<syn::ItemEnum>);
        impl<'ast> syn::visit::Visit<'ast> for Collect {
            fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
                self.0.push(item.clone());
            }
        }
        let mut collect = Collect(Vec::new());
        syn::visit::Visit::visit_file(&mut collect, &syn::parse_file(source).unwrap());
        let mut item = collect.0.swap_remove(nth);
        // rustc hands the derive its item without the invoking `#[derive]`.
        item.attrs.retain(|attr| !attr.path().is_ident("derive"));
        syn::parse2(quote::quote!(#item)).unwrap()
    }

    /// L4: same-named enums in different scopes — each derive input gets
    /// the `serde_repr` derives of ITS declaration (matched by position),
    /// never those of the first same-named enum.
    #[rstest]
    #[case::second_has_no_repr(
        "mod a { #[derive(Serialize_repr, Schema)] #[repr(u8)] enum E { A } }\nmod b { #[derive(Serialize, Schema)] enum E { A } }",
        1,
        &[]
    )]
    #[case::second_has_repr(
        "mod a { #[derive(Serialize, Schema)] enum E { A } }\nfn f() { #[derive(Deserialize_repr, Schema)] #[repr(u8)] enum E { A } }",
        1,
        &["Deserialize_repr"]
    )]
    #[case::first_has_repr(
        "mod a { #[derive(Serialize_repr, Schema)] #[repr(u8)] enum E { A } }\nmod b { enum E { A } }",
        0,
        &["Serialize_repr"]
    )]
    fn same_named_enums_are_matched_by_position(
        #[case] source: &str,
        #[case] nth: usize,
        #[case] expected: &[&str],
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, source).unwrap();
        let input = enum_at(source, nth);
        let found: Vec<&str> = with_serde_repr_derives(&input, Some(&path))
            .map(|input| {
                SERDE_REPR_DERIVES
                    .into_iter()
                    .filter(|name| derives(&input.attrs, name))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(found, expected);
    }

    /// Several same-named enums and none at the input's position (the item
    /// moved, or the file is unparseable): nothing is borrowed.
    #[rstest]
    #[case::ambiguous_moved(
        "mod a { #[derive(Serialize_repr)] #[repr(u8)] enum E { A } }\nmod b { #[derive(Serialize_repr)] #[repr(u8)] enum E { A } }"
    )]
    #[case::unparseable("#[derive(Serialize_repr)] enum E {")]
    fn ambiguous_or_unparseable_source_adds_nothing(#[case] source: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, source).unwrap();
        let input: syn::DeriveInput = syn::parse_str("#[repr(u8)] enum E { A }").unwrap();
        assert!(with_serde_repr_derives(&input, Some(&path)).is_none());
    }

    #[test]
    fn structs_and_missing_files_are_untouched() {
        let input: syn::DeriveInput = syn::parse_str("struct S;").unwrap();
        assert!(with_serde_repr_derives(&input, Some(Path::new("missing.rs"))).is_none());
        let input: syn::DeriveInput = syn::parse_str("enum E { A }").unwrap();
        assert!(with_serde_repr_derives(&input, None).is_none());
    }
}
