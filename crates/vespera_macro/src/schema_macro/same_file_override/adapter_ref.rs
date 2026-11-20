//! The shape and the `$ref` name of an explicitly listed relation adapter DTO
//! (`relation_adapters` / `schema_type!` adapter fields).
//!
//! The wrapper `$ref`s the DTO's component, and it names the DTO struct
//! without type arguments, so both facts are checked here: the component name
//! is the one the storage entry registers (R33-SM3-M1), and a generic DTO is
//! refused with a spanned error instead of E0107 in the generated code
//! (R33-SM3-L1).

use syn::punctuated::Punctuated;

use crate::metadata::StructMetadata;
use crate::schema_macro::cfg_eval::{Predicate, statically_active};

/// The component name the wrapper's `$ref` names: the DTO's `#[schema(name =
/// ..)]` override when it has one (a `cfg_attr` one only when its predicate is
/// statically active, see [`active_cfg_attr_schema_name`]); else, for a DTO
/// taken from the crate-wide storage (`stored`), the storage entry's component
/// key (`name`), which differs from the Rust identifier when the entry was
/// registered under an explicit name (`rust_ident` is then the identifier;
/// R33-SM3-M1); else the Rust name.
pub(super) fn schema_ref_name(
    stored: Option<&StructMetadata>,
    dto_struct: &syn::ItemStruct,
    dto_name: &str,
) -> String {
    if let Some(name) = crate::schema_impl::extract_schema_name_attr(&dto_struct.attrs) {
        return name;
    }
    stored.map_or_else(
        || active_cfg_attr_schema_name(&dto_struct.attrs).unwrap_or_else(|| dto_name.to_string()),
        |stored| stored.name.clone(),
    )
}

/// The `schema(name = ..)` a `cfg_attr(p, schema(..))` of `attrs` sets, when
/// `p` is statically active. An undecided or false predicate sets nothing: the
/// Rust name then stands, the same as a plain call-site struct (R34-SM3-L1).
fn active_cfg_attr_schema_name(attrs: &[syn::Attribute]) -> Option<String> {
    let schema: Vec<syn::Attribute> = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg_attr"))
        .filter_map(|attr| {
            let (predicate, metas) = attr
                .parse_args_with(|input: syn::parse::ParseStream| {
                    let predicate: Predicate = input.parse()?;
                    input.parse::<syn::Token![,]>()?;
                    Ok((
                        predicate,
                        Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(input)?,
                    ))
                })
                .ok()?;
            statically_active(&predicate).then_some(metas)
        })
        .flatten()
        .filter(|meta| meta.path().is_ident("schema"))
        .map(|meta| syn::parse_quote!(#[#meta]))
        .collect();
    crate::schema_impl::extract_schema_name_attr(&schema)
}

/// Refuses an adapter DTO with type or lifetime parameters: the wrapper names
/// it bare (`UserInArticle { .. }`), which does not compile for a generic
/// struct (R33-SM3-L1).
pub(super) fn reject_generic_adapter(
    field_name: &str,
    adapter_name: &syn::Ident,
    dto_struct: &syn::ItemStruct,
) -> syn::Result<()> {
    if dto_struct.generics.params.is_empty() {
        return Ok(());
    }
    Err(syn::Error::new_spanned(
        adapter_name,
        format!(
            "schema_type!: relation field `{field_name}` explicitly requested adapter `{adapter_name}`, which is generic; the wrapper names it without type arguments (E0107), so give the adapter a concrete, non-generic struct"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::explicit_adapter_struct;
    use super::*;

    /// R33-SM3-M1: a storage DTO whose component is `ArticleUser` but whose
    /// Rust identifier is `UserInArticle` is `$ref`ed as `ArticleUser`.
    #[test]
    fn storage_adapter_refs_its_component_name() {
        let mut meta = StructMetadata::new(
            "ArticleUser".to_string(),
            "struct UserInArticle { pub id: i32 }".to_string(),
        );
        meta.rust_ident = Some("UserInArticle".to_string());
        let storage = HashMap::from([("ArticleUser".to_string(), meta)]);
        let ident = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());
        let (dto_name, schema_ref, _) =
            explicit_adapter_struct("author", &ident, &storage).unwrap();
        assert_eq!(dto_name, "UserInArticle");
        assert_eq!(schema_ref, "ArticleUser");
    }

    /// A call-site struct keeps the `#[schema(name = ..)]` path (the same
    /// file's override) and, without one, its own Rust name.
    #[rstest::rstest]
    #[case::schema_name_attribute("#[schema(name = \"Named\")] struct Dto { id: i32 }", "Named")]
    #[case::rust_name("struct Dto { id: i32 }", "Dto")]
    fn call_site_adapter_refs_its_own_name(#[case] source: &str, #[case] expected: &str) {
        let dto_struct: syn::ItemStruct = syn::parse_str(source).unwrap();
        let ref_name = schema_ref_name(None, &dto_struct, "Dto");
        assert_eq!(ref_name, expected);
    }

    /// R33-SM3-L1: a generic adapter (type or lifetime parameter) is refused
    /// with a spanned error; a non-generic one passes.
    #[test]
    fn generic_adapter_is_refused_and_a_plain_one_passes() {
        let ident = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());
        let generic: syn::ItemStruct =
            syn::parse_str("struct UserInArticle<T> { pub value: T }").unwrap();
        let error = reject_generic_adapter("author", &ident, &generic).unwrap_err();
        assert!(error.to_string().contains("which is generic"), "{error}");

        let lifetime: syn::ItemStruct =
            syn::parse_str("struct UserInArticle<'a> { pub name: &'a str }").unwrap();
        assert!(reject_generic_adapter("author", &ident, &lifetime).is_err());

        let plain: syn::ItemStruct =
            syn::parse_str("struct UserInArticle { pub id: i32 }").unwrap();
        assert!(reject_generic_adapter("author", &ident, &plain).is_ok());
    }

    /// R33-SM3-L1 through the whole lookup: a generic storage DTO is refused
    /// by `explicit_adapter_struct` itself.
    #[test]
    fn explicit_adapter_refuses_a_generic_storage_dto() {
        let meta = StructMetadata::new(
            "__VesperaGenericAdapter".to_string(),
            "struct __VesperaGenericAdapter<T> { pub value: T }".to_string(),
        );
        let storage = HashMap::from([("__VesperaGenericAdapter".to_string(), meta)]);
        let ident = syn::Ident::new("__VesperaGenericAdapter", proc_macro2::Span::call_site());
        let Err(error) = explicit_adapter_struct("author", &ident, &storage) else {
            panic!("a generic adapter should be refused")
        };
        assert!(error.to_string().contains("which is generic"), "{error}");
    }

    /// R34-SM3-M1: a call-site adapter that does not derive `Schema` has no
    /// component to `$ref`; it is refused with the spanned error instead of a
    /// dangling `$ref`.
    #[test]
    fn explicit_adapter_refuses_an_underived_call_site_struct() {
        let ident = syn::Ident::new(
            "__VesperaSameFileLookupFixture",
            proc_macro2::Span::call_site(),
        );
        let Err(error) = explicit_adapter_struct("author", &ident, &HashMap::new()) else {
            panic!("an underived call-site adapter should be refused")
        };
        assert!(
            error.to_string().contains("does not derive `Schema`"),
            "{error}"
        );
    }

    /// R34-SM3-L1: a `cfg_attr` rename counts only while its predicate is
    /// statically active; a stored entry's component key is the one `$ref`ed.
    #[rstest::rstest]
    #[case::active_cfg_attr(
        "#[cfg_attr(all(), schema(name = \"Renamed\"))] struct Dto { id: i32 }",
        "Renamed"
    )]
    #[case::undecided_cfg_attr(
        "#[cfg_attr(unix, schema(name = \"Renamed\"))] struct Dto { id: i32 }",
        "Dto"
    )]
    #[case::false_cfg_attr(
        "#[cfg_attr(any(), schema(name = \"Renamed\"))] struct Dto { id: i32 }",
        "Dto"
    )]
    #[case::other_cfg_attr("#[cfg_attr(all(), derive(Debug))] struct Dto { id: i32 }", "Dto")]
    fn cfg_attr_schema_name_follows_its_predicate(#[case] source: &str, #[case] expected: &str) {
        let dto_struct: syn::ItemStruct = syn::parse_str(source).unwrap();
        assert_eq!(schema_ref_name(None, &dto_struct, "Dto"), expected);
    }

    #[test]
    fn stored_component_key_wins_over_a_cfg_attr_rename() {
        let dto_struct: syn::ItemStruct =
            syn::parse_str("#[cfg_attr(all(), schema(name = \"Renamed\"))] struct Dto { id: i32 }")
                .unwrap();
        let stored = StructMetadata::new("Stored".to_string(), String::new());
        assert_eq!(schema_ref_name(Some(&stored), &dto_struct, "Dto"), "Stored");
    }

    /// R34-SM3-L2: a `cfg_attr` conversion key is honoured only while its
    /// predicate is statically active; an undecided one is dropped (and so is
    /// it from the proxy's own attributes).
    #[rstest::rstest]
    #[case::active(
        "#[cfg_attr(all(), serde(from = \"String\"))] struct Dto { id: i32 }",
        true
    )]
    #[case::undecided(
        "#[cfg_attr(unix, serde(from = \"String\"))] struct Dto { id: i32 }",
        false
    )]
    fn cfg_attr_conversion_key_is_honoured_only_when_active(
        #[case] source: &str,
        #[case] honoured: bool,
    ) {
        let dto_struct: syn::ItemStruct = syn::parse_str(source).unwrap();
        let serde = super::super::dto_serde(&dto_struct);
        assert_eq!(serde.convert_from.is_some(), honoured);
        assert!(serde.proxy_attrs.is_empty());
    }
}
