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
    let content = crate::schema_macro::file_cache::get_file_content(path)?;
    if !content.contains("_repr") {
        return None;
    }
    let file = crate::schema_macro::file_cache::get_parsed_file(path)?;
    let source_derives = find_enum_derives(&file.items, &input.ident)?;
    let missing: Vec<&str> = SERDE_REPR_DERIVES
        .into_iter()
        .filter(|name| derives(&source_derives, name) && !derives(&input.attrs, name))
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

/// The attributes of the enum named `ident` (inline modules searched too).
fn find_enum_derives(items: &[syn::Item], ident: &syn::Ident) -> Option<Vec<syn::Attribute>> {
    items.iter().find_map(|item| match item {
        syn::Item::Enum(item) if item.ident == *ident && derives_any_repr(&item.attrs) => {
            Some(item.attrs.clone())
        }
        syn::Item::Mod(module) => module
            .content
            .as_ref()
            .and_then(|(_, items)| find_enum_derives(items, ident)),
        _ => None,
    })
}

fn derives_any_repr(attrs: &[syn::Attribute]) -> bool {
    SERDE_REPR_DERIVES.iter().any(|name| derives(attrs, name))
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

    #[test]
    fn structs_and_missing_files_are_untouched() {
        let input: syn::DeriveInput = syn::parse_str("struct S;").unwrap();
        assert!(with_serde_repr_derives(&input, Some(Path::new("missing.rs"))).is_none());
        let input: syn::DeriveInput = syn::parse_str("enum E { A }").unwrap();
        assert!(with_serde_repr_derives(&input, None).is_none());
    }
}
