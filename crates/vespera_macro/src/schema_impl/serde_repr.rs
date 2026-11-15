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
    let source_derives = source_derives(path, &input.ident, &SERDE_REPR_DERIVES, false)?;
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

/// A `#[derive(Multipart)]` struct (that derive is as invisible to `Schema`
/// as `serde_repr`'s): its `Vec<T>` fields without a default of their own
/// get `#[form_data(default)]` in the stored definition. The Multipart
/// derive reads "no parts" as `[]`, so the request schema must not list
/// them as `required` (the schema parser reads that attribute).
pub(super) fn with_multipart_vec_defaults(
    input: &syn::DeriveInput,
    call_site_file: Option<&Path>,
) -> Option<syn::DeriveInput> {
    let syn::Data::Struct(data) = &input.data else {
        return None;
    };
    let path = call_site_file?;
    let file = crate::schema_macro::file_cache::get_file_content(path)?;
    if !file.contains("Multipart")
        || source_derives(path, &input.ident, &["Multipart"], true)?.is_empty()
    {
        return None;
    }
    let mut data = data.clone();
    let mut changed = false;
    for field in data.fields.iter_mut().filter(|field| {
        crate::multipart_impl::is_vec_type(&field.ty)
            && crate::parser::extract_default(&field.attrs).is_none()
            && !crate::multipart_impl::has_form_data_default(&field.attrs)
    }) {
        field.attrs.push(syn::parse_quote!(#[form_data(default)]));
        changed = true;
    }
    changed.then(|| syn::DeriveInput {
        data: syn::Data::Struct(data),
        ..input.clone()
    })
}

/// The derives among `names` of THE source declaration of the enum (or,
/// with `is_struct`, the struct) `ident`.
///
/// Same-named enums may live in different inline `mod`s (or fn bodies) of one
/// file, so the declaration is matched by the ident's `line:column`; when no
/// declaration sits there (the item moved under a long-lived proc-macro
/// server), a single same-named enum is still unambiguous.  Several candidates
/// and no positional match → `None` (never borrow another enum's derives).
///
/// The declarations come from the file's [`Declarations`] index, read off a
/// parse with proc-macro2's fallback forced so its spans carry real
/// positions (compiler spans of re-parsed text all point at the call site)
/// and kept while the file is unchanged: every enum / `Multipart` struct
/// derive of a file otherwise re-parsed it.
fn source_derives(
    file: &Path,
    ident: &syn::Ident,
    names: &[&'static str],
    is_struct: bool,
) -> Option<Vec<&'static str>> {
    let declarations = declarations(file)?;
    let start = ident.span().start();
    let ident = ident.to_string();
    let found: Vec<&Declaration> = declarations
        .iter()
        .filter(|declaration| declaration.is_struct == is_struct && declaration.ident == ident)
        .collect();
    let index = found
        .iter()
        .position(|declaration| declaration.position == (start.line, start.column))
        .or_else(|| (found.len() == 1).then_some(0))?;
    Some(
        names
            .iter()
            .copied()
            .filter(|name| found[index].derives.contains(name))
            .collect(),
    )
}

/// Every derive [`source_derives`] can be asked about.
const KNOWN_DERIVES: [&str; 3] = ["Serialize_repr", "Deserialize_repr", "Multipart"];

/// One enum / struct declaration of a file.
struct Declaration {
    is_struct: bool,
    ident: String,
    /// The ident's `(line, column)`.
    position: (usize, usize),
    /// Its derives among [`KNOWN_DERIVES`].
    derives: Vec<&'static str>,
}

/// A file's declarations (`None`: it does not parse).
struct Declarations(Option<std::rc::Rc<Vec<Declaration>>>);

fn declarations(file: &Path) -> Option<std::rc::Rc<Vec<Declaration>>> {
    #[derive(Default)]
    struct Finder(Vec<Declaration>);
    impl Finder {
        fn record(&mut self, ident: &syn::Ident, attrs: &[syn::Attribute], is_struct: bool) {
            let start = ident.span().start();
            self.0.push(Declaration {
                is_struct,
                ident: ident.to_string(),
                position: (start.line, start.column),
                derives: KNOWN_DERIVES
                    .iter()
                    .copied()
                    .filter(|name| derives(attrs, name))
                    .collect(),
            });
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Finder {
        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            self.record(&item.ident, &item.attrs, false);
            syn::visit::visit_item_enum(self, item);
        }
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            self.record(&item.ident, &item.attrs, true);
            syn::visit::visit_item_struct(self, item);
        }
    }
    use crate::schema_macro::file_cache;
    let cached = file_cache::derived(file, |content| {
        Declarations(file_cache::with_fallback_parse(content, |file| {
            let mut finder = Finder::default();
            syn::visit::Visit::visit_file(&mut finder, file);
            std::rc::Rc::new(finder.0)
        }))
    })?;
    cached.0.clone()
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

    /// L9 (round 12): the Multipart derive fills an absent `Vec<T>` part with
    /// `[]`, so its fields (without a default of their own) are stored with
    /// `#[form_data(default)]`: the request schema used to require them.
    #[rstest]
    #[case::multipart(
        "#[derive(Multipart, Schema)] struct U { files: Vec<FieldData<u8>>, tags: std::vec::Vec<String>, \
         #[serde(default)] own: Vec<u8>, #[form_data(default)] kept: Vec<u8>, name: String, opt: Option<Vec<u8>> }",
        Some(vec!["files", "tags"])
    )]
    // L1 (round 13): the derive's own predicate decides: `alloc::vec::Vec`
    // is a repeated part, another crate's `Vec` a single (required) one.
    #[case::shared_vec_predicate(
        "#[derive(Multipart)] struct U { files: alloc::vec::Vec<u8>, fixed: heapless::Vec<u32, 8> }",
        Some(vec!["files"])
    )]
    #[case::qualified_derive("#[derive(vespera::Multipart)] struct U { files: Vec<u8> }", Some(vec!["files"]))]
    #[case::no_vec_field("#[derive(Multipart)] struct U { name: String }", None)]
    #[case::not_multipart(
        "#[derive(Schema)] struct U { files: Vec<u8> } struct Multipart;",
        None
    )]
    #[case::other_struct("#[derive(Multipart)] struct V { files: Vec<u8> }", None)]
    fn multipart_vec_fields_get_a_form_data_default(
        #[case] source: &str,
        #[case] expected: Option<Vec<&str>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, source).unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let file = syn::parse_file(source).unwrap();
        let item = file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Struct(item) if item.ident == "U" => Some(item.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                syn::parse_quote!(
                    struct U {
                        files: Vec<u8>,
                    }
                )
            });
        let mut input: syn::DeriveInput = item.into();
        input.attrs.retain(|attr| !attr.path().is_ident("derive"));
        let defaulted = with_multipart_vec_defaults(&input, Some(&path)).map(|input| {
            let syn::Data::Struct(data) = input.data else {
                unreachable!()
            };
            data.fields
                .iter()
                .filter(|field| {
                    field
                        .attrs
                        .last()
                        .is_some_and(|attr| attr.path().is_ident("form_data"))
                        && crate::multipart_impl::has_form_data_default(&field.attrs)
                        && field.ident.as_ref().is_some_and(|ident| ident != "kept")
                })
                .map(|field| field.ident.as_ref().unwrap().to_string())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            defaulted,
            expected.map(|names| names.into_iter().map(String::from).collect())
        );
    }

    #[test]
    fn multipart_vec_defaults_need_a_struct_and_a_file() {
        let input: syn::DeriveInput = syn::parse_str("enum E { A }").unwrap();
        assert!(with_multipart_vec_defaults(&input, Some(Path::new("missing.rs"))).is_none());
        let input: syn::DeriveInput = syn::parse_str("struct S { v: Vec<u8> }").unwrap();
        assert!(with_multipart_vec_defaults(&input, None).is_none());
        assert!(with_multipart_vec_defaults(&input, Some(Path::new("missing.rs"))).is_none());
    }

    #[test]
    fn structs_and_missing_files_are_untouched() {
        let input: syn::DeriveInput = syn::parse_str("struct S;").unwrap();
        assert!(with_serde_repr_derives(&input, Some(Path::new("missing.rs"))).is_none());
        let input: syn::DeriveInput = syn::parse_str("enum E { A }").unwrap();
        assert!(with_serde_repr_derives(&input, None).is_none());
    }
}
