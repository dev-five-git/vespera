//! `from_model` implementation generation
//!
//! Generates async `from_model` implementations for `SeaORM` models with relations.

use proc_macro2::TokenStream;
use quote::quote;

mod generate;

pub use generate::generate_from_model_with_relations;

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
pub fn build_entity_path_from_schema_path(
    schema_path: &TokenStream,
    _source_module_path: &[String],
) -> TokenStream {
    // Parse the schema path, replace "Schema" with "Entity", and build Idents in one pass
    let path_str = schema_path.to_string();
    let path_idents: Vec<syn::Ident> = path_str
        .split("::")
        .map(|s| {
            let s = s.trim();
            let name = if s == "Schema" { "Entity" } else { s };
            syn::Ident::new(name, proc_macro2::Span::call_site())
        })
        .collect();

    quote! { #(#path_idents)::* }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Entity-path derivation: the rewritten PATH is the whole contract —
    // each case snapshots the exact token output (e.g. `Schema` tail must
    // become `Entity`, all module segments preserved) instead of probing
    // substrings.  Snapshot names are explicit because insta's
    // auto-naming shuffles across parallel rstest cases.
    #[rstest]
    #[case::crate_qualified("entity_path_crate_qualified", quote! { crate::models::user::Schema })]
    #[case::simple_module("entity_path_simple_module", quote! { user::Schema })]
    #[case::deeply_nested(
        "entity_path_deeply_nested",
        quote! { crate::api::models::entities::user::Schema }
    )]
    #[case::single_segment("entity_path_single_segment", quote! { Schema })]
    fn build_entity_path_from_schema_path_snapshot(
        #[case] snapshot_name: &str,
        #[case] schema_path: TokenStream,
    ) {
        insta::assert_snapshot!(
            snapshot_name,
            build_entity_path_from_schema_path(&schema_path, &[]).to_string()
        );
    }
}
