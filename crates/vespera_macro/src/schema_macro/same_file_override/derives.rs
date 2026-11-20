//! Whether a call-site struct derives `Schema`: a plain `derive(..)`, or one
//! behind a `cfg_attr(p, ..)` unless `p` is known false (R32-SM3-L2). A
//! `cfg_attr`-gated derive is taken as active, the conservative side for the
//! refusal of an adapter that does not derive `Schema`.

use syn::parse::ParseStream;
use syn::punctuated::Punctuated;

use crate::schema_macro::cfg_eval::{Predicate, statically_inactive_predicate};
use crate::schema_macro::file_cache;

type DerivePaths = Punctuated<syn::Path, syn::Token![,]>;

/// Whether the struct `definition` has `Schema` in one of its `derive`s.
pub(super) fn derives_schema(definition: &str) -> bool {
    let Ok(item) = file_cache::parse_struct_cached(definition) else {
        return false;
    };
    derive_paths(&item.attrs)
        .iter()
        .any(|path| path.segments.last().is_some_and(|s| s.ident == "Schema"))
}

/// The paths of every `derive(..)` among `attrs`, plain or under an active
/// `cfg_attr`.
fn derive_paths(attrs: &[syn::Attribute]) -> Vec<syn::Path> {
    attrs
        .iter()
        .flat_map(|attr| {
            if attr.path().is_ident("derive") {
                paths(attr.parse_args_with(DerivePaths::parse_terminated))
            } else if attr.path().is_ident("cfg_attr") {
                cfg_attr_derive_paths(attr)
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// The `derive(..)` items of `cfg_attr(p, ..)` when `p` is not known false.
fn cfg_attr_derive_paths(attr: &syn::Attribute) -> Vec<syn::Path> {
    let Ok((predicate, metas)) = attr.parse_args_with(cfg_attr_parts) else {
        return Vec::new();
    };
    if statically_inactive_predicate(&predicate) {
        return Vec::new();
    }
    metas
        .iter()
        .filter(|meta| meta.path().is_ident("derive"))
        .flat_map(|meta| {
            meta.require_list().map_or_else(
                |_| Vec::new(),
                |list| paths(list.parse_args_with(DerivePaths::parse_terminated)),
            )
        })
        .collect()
}

/// The predicate and the items of `cfg_attr(predicate, items..)`.
fn cfg_attr_parts(
    input: ParseStream<'_>,
) -> syn::Result<(Predicate, Punctuated<syn::Meta, syn::Token![,]>)> {
    let predicate: Predicate = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    Ok((predicate, Punctuated::parse_terminated(input)?))
}

/// The paths of a parsed derive list (an unparseable one derives nothing).
fn paths(parsed: syn::Result<DerivePaths>) -> Vec<syn::Path> {
    parsed
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::super::choose_same_file;
    use super::*;
    use crate::metadata::StructMetadata;

    #[rstest]
    #[case::plain("#[derive(Debug, vespera::Schema)] struct S { id: i32 }", true)]
    #[case::cfg_attr_true(
        "#[cfg_attr(all(), derive(vespera::Schema))] struct S { id: i32 }",
        true
    )]
    #[case::cfg_attr_unknown(
        "#[cfg_attr(feature = \"openapi\", derive(vespera::Schema))] struct S { id: i32 }",
        true
    )]
    #[case::cfg_attr_false("#[cfg_attr(any(), derive(vespera::Schema))] struct S {}", false)]
    #[case::cfg_attr_without_schema("#[cfg_attr(all(), derive(Debug))] struct S {}", false)]
    #[case::cfg_attr_without_derive("#[cfg_attr(all(), serde(default))] struct S {}", false)]
    #[case::cfg_attr_unparseable("#[cfg_attr(feature = \"openapi\")] struct S {}", false)]
    #[case::cfg_attr_bare_derive("#[cfg_attr(all(), derive)] struct S {}", false)]
    #[case::unparseable_derive("#[derive(1)] struct S {}", false)]
    #[case::other_attr("#[serde(rename_all = \"camelCase\")] struct S {}", false)]
    fn derives_schema_sees_plain_and_cfg_attr_derives(
        #[case] definition: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(derives_schema(definition), expected);
    }

    /// A call-site struct whose `Schema` derive sits behind `cfg_attr` is the
    /// listed adapter, not the storage's copy (its fields come from the file).
    #[test]
    fn cfg_attr_derived_file_struct_wins_over_storage() {
        let stored = StructMetadata::new(
            "UserInArticle".to_string(),
            "struct UserInArticle { pub name: String }".to_string(),
        );
        let file = "#[cfg_attr(feature = \"openapi\", derive(vespera::Schema))] \
                    struct UserInArticle { pub id: i32 }"
            .to_string();
        let chosen = choose_same_file("UserInArticle", Some(file.clone()), Some(&stored));
        assert_eq!(chosen.map(|meta| meta.definition.clone()), Some(file));
    }
}
