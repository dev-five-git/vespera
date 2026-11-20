//! `from_model` implementation generation
//!
//! Generates async `from_model` implementations for `SeaORM` models with relations.

use proc_macro2::{TokenStream, TokenTree};

mod generate;
mod loads;
mod target_shape;

pub use generate::generate_from_model_with_relations;
pub(super) use loads::{source_column_read, source_field_named};

/// One emitted DTO field and the source field it is built from (`From`,
/// the container default, `from_model`).
pub struct FieldMapping {
    pub new_ident: syn::Ident,
    pub source_ident: syn::Ident,
    /// `partial` wrapped the field into `Option`.
    pub wrapped: bool,
    /// The field's entry among the relation fields (a SeaORM relation):
    /// `#[cfg]` twins share a name, so they are paired by position.
    pub relation: Option<usize>,
    /// The source field's `#[cfg(..)]` attributes, repeated on everything
    /// that touches it.
    pub cfg: TokenStream,
    /// The source field's `#[cfg]` is neither known true nor known false: the
    /// compiler may drop the emitted field (R34-SM5-2).
    pub cfg_undecided: bool,
}

/// A parameter of the generated `from_model`, named so no relation field
/// (whose loaded value becomes a local of the field's name) can shadow it.
pub(super) struct Param(&'static str);

impl quote::ToTokens for Param {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend([TokenTree::Ident(proc_macro2::Ident::new(
            self.0,
            proc_macro2::Span::call_site(),
        ))]);
    }
}

/// The source model parameter.
pub(super) const MODEL: Param = Param("__vespera_model");
/// The database connection parameter.
pub(super) const DB: Param = Param("__vespera_db");

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

/// The `Model` path behind a normalized relation `Schema` path: only the final
/// segment is rewritten (`crate::SchemaDefs::user::Schema` →
/// `crate::SchemaDefs::user::Model`) — a whole-string `replace` also renamed
/// module segments starting with `Schema`, so the model lookup missed.
pub(super) fn model_path_of(schema_path: &str) -> String {
    match schema_path.rsplit_once("::") {
        Some((module, last)) if last.starts_with("Schema") => {
            format!("{module}::Model{}", &last["Schema".len()..])
        }
        _ => schema_path.to_owned(),
    }
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

    #[rstest]
    #[case::plain("crate::models::user::Schema", "crate::models::user::Model")]
    #[case::schema_module("crate::SchemaDefs::user::Schema", "crate::SchemaDefs::user::Model")]
    #[case::custom_name("crate::user::UserSchema", "crate::user::UserSchema")]
    #[case::bare("Schema", "Schema")]
    fn model_path_of_rewrites_only_the_last_segment(#[case] schema: &str, #[case] model: &str) {
        assert_eq!(model_path_of(schema), model);
    }
}
