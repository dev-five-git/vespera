//! `from_model` implementation generation
//!
//! Generates async `from_model` implementations for `SeaORM` models with relations.

use proc_macro2::{TokenStream, TokenTree};

mod generate;
mod loads;

pub use generate::generate_from_model_with_relations;

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
///
/// Token-level, so raw identifiers (`crate::r#type::Schema`) survive.
pub fn build_entity_path_from_schema_path(
    schema_path: &TokenStream,
    _source_module_path: &[String],
) -> TokenStream {
    replace_last_ident(schema_path, Some("Schema"), "Entity")
}

/// `path` with its last identifier replaced by `to` (only when it is
/// `from`, if given).
pub fn replace_last_ident(path: &TokenStream, from: Option<&str>, to: &str) -> TokenStream {
    let mut tokens: Vec<TokenTree> = path.clone().into_iter().collect();
    if let Some(last) = tokens
        .iter()
        .rposition(|token| matches!(token, TokenTree::Ident(_)))
        && from.is_none_or(|from| tokens[last].to_string() == from)
    {
        tokens[last] = TokenTree::Ident(syn::Ident::new(to, tokens[last].span()));
    }
    tokens.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use quote::quote;
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
