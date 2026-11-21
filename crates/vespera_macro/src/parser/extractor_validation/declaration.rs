//! Per-declaration `Schema` status for the extractor validator.
//!
//! The validator decides whether a route-folder type is documentable from the
//! attributes of its own declaration, not from a crate-wide set of `Schema`
//! names: a `Schema` type of the same name in another module must not hide a
//! non-`Schema` type that a handler actually uses.

use std::collections::HashSet;

use syn::{Meta, Token, punctuated::Punctuated};

/// Whether `attrs` derive `Schema` (`Schema` or `vespera::Schema`), directly or
/// under a `cfg_attr`. A conditional derive counts as `Schema`: a false
/// "not `Schema`" would reject a documentable type, which is the worse error.
pub(super) fn declares_schema(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let Ok(list) = attr.meta.require_list() else {
            return false;
        };
        if list.path.is_ident("derive") {
            derive_names_schema(list)
        } else if list.path.is_ident("cfg_attr") {
            list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|parts| {
                    parts.iter().any(|part| {
                        part.require_list().is_ok_and(|inner| {
                            inner.path.is_ident("derive") && derive_names_schema(inner)
                        })
                    })
                })
        } else {
            false
        }
    })
}

/// Whether the `derive(..)` list names a `Schema` derive.
fn derive_names_schema(list: &syn::MetaList) -> bool {
    list.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
        .is_ok_and(|paths| {
            paths.iter().any(|path| {
                path.segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Schema")
            })
        })
}

/// The names of the structs and enums `ast` declares that do not derive
/// `Schema` in their own declaration.
pub(super) fn non_schema_type_names(ast: &syn::File) -> HashSet<String> {
    ast.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(s) if !declares_schema(&s.attrs) => Some(s.ident.to_string()),
            syn::Item::Enum(e) if !declares_schema(&e.attrs) => Some(e.ident.to_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::derive_schema("#[derive(Debug, Schema)] struct S;", true)]
    #[case::derive_qualified_schema("#[derive(vespera::Schema)] struct S;", true)]
    #[case::derive_without_schema("#[derive(Debug)] struct S;", false)]
    #[case::cfg_attr_derive_schema("#[cfg_attr(test, derive(Schema))] struct S;", true)]
    #[case::cfg_attr_without_derive("#[cfg_attr(test, allow(dead_code))] struct S;", false)]
    #[case::plain_list_attr("#[allow(dead_code)] struct S;", false)]
    #[case::name_value_attr("#[doc = \"x\"] struct S;", false)]
    #[case::path_attr("#[inline] struct S;", false)]
    fn schema_derive_is_read_from_the_declaration(#[case] src: &str, #[case] schema: bool) {
        let item: syn::ItemStruct = syn::parse_str(src).expect("struct parses");
        assert_eq!(declares_schema(&item.attrs), schema, "{src}");
    }

    #[test]
    fn non_schema_names_cover_structs_and_enums_only() {
        let ast: syn::File = syn::parse_quote! {
            #[derive(Schema)] pub struct Documented { pub a: i32 }
            pub struct Plain { pub a: i32 }
            #[derive(Debug)] pub enum Kind { A }
            pub fn helper() {}
        };
        assert_eq!(
            non_schema_type_names(&ast),
            HashSet::from(["Plain".to_string(), "Kind".to_string()])
        );
    }
}
