//! Async `from_model` impl generation for SeaORM models with
//! relations (inline types for circular relations, FK lookups).

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::super::{
    circular::generate_inline_type_construction,
    file_cache::{
        get_circular_analysis, get_module_path_from_schema_path, get_struct_from_schema_path,
    },
    seaorm::RelationFieldInfo,
    type_utils::normalize_token_str,
};
use super::loads::relation_load;
use super::target_shape::{TargetShape, target_shape};
use super::{DB, MODEL};

/// Generate `from_model` impl for `SeaORM` Model WITH relations (async version).
///
/// A relation whose target leads back to the source (circular, including
/// self references and longer cycles) has an inline type that excludes
/// every relation (`generate_type`), constructed from the loaded row here,
/// so the generated loads never recurse.
///
/// ```ignore
/// impl NewType {
///     pub async fn from_model(
///         __vespera_model: SourceType,
///         __vespera_db: &sea_orm::DatabaseConnection,
///     ) -> Result<Self, sea_orm::DbErr> {
///         // Load related entities
///         let user = ...one(__vespera_db).await?;
///         let tags = ...all(__vespera_db).await?;
///
///         Ok(Self {
///             id: __vespera_model.id,
///             // Inline type of a circular relation:
///             user: user.map(|r| Box::new(NewType_User { id: r.id, ... })),
///             tags: tags.into_iter().map(|r| <tag::Schema as From<_>>::from(r)).collect(),
///         })
///     }
/// }
/// ```
///
/// The parameters are named `__vespera_model` / `__vespera_db` so a relation
/// field called `model` or `db` (whose loaded value is bound to a local of
/// that name) cannot shadow them.
///
/// # Errors
///
/// A `#[sea_orm(...)]` string value that must become an identifier
/// (`relation_enum`, `from`, or the target entity's `from` reached through
/// `via_rel`) but is not one (e.g. `from = "author id"`) is reported as a
/// `syn::Error` spanned on `source_type`, instead of panicking the macro.
pub fn generate_from_model_with_relations(
    new_type_name: &syn::Ident,
    source_type: &Type,
    field_mappings: &[(syn::Ident, syn::Ident, bool, bool)],
    relation_fields: &[RelationFieldInfo],
    source_module_path: &[String],
    field_cfgs: &HashMap<String, TokenStream>,
) -> syn::Result<TokenStream> {
    // `#[cfg(..)]` of a source field, repeated on everything touching it.
    let cfg_of = |ident: &syn::Ident| {
        field_cfgs
            .get(&crate::parser::strip_raw_prefix_owned(ident.to_string()))
            .cloned()
            .unwrap_or_default()
    };

    // Build relation loading statements
    let relation_loads: Vec<TokenStream> = relation_fields
        .iter()
        .map(|rel| {
            let cfg = cfg_of(&rel.field_name);
            relation_load(rel, source_type, source_module_path).map(|load| quote! { #cfg #load })
        })
        .collect::<syn::Result<_>>()?;

    // Pre-build relation lookup for O(1) access in field assignments loop
    let relation_by_name: HashMap<&syn::Ident, &RelationFieldInfo> = relation_fields
        .iter()
        .map(|rel| (&rel.field_name, rel))
        .collect();

    let field_assignments: Vec<TokenStream> = field_mappings
        .iter()
        .map(|(new_ident, source_ident, wrapped, is_relation)| {
            let cfg = cfg_of(source_ident);
            let value = if *is_relation {
                relation_by_name.get(source_ident).map_or_else(
                    || quote! { Default::default() },
                    |rel| relation_value(rel, source_ident, source_module_path),
                )
            } else if *wrapped {
                quote! { Some(#MODEL.#source_ident) }
            } else {
                quote! { #MODEL.#source_ident }
            };
            quote! { #cfg #new_ident: #value }
        })
        .collect();

    Ok(quote! {
        #[allow(deprecated)]
        impl #new_type_name {
            pub async fn from_model(
                #MODEL: #source_type,
                #DB: &sea_orm::DatabaseConnection,
            ) -> Result<Self, sea_orm::DbErr> {
                #(#relation_loads)*

                Ok(Self {
                    #(#field_assignments),*
                })
            }
        }
    })
}

/// The value of one relation field, built from the loaded local
/// `source_ident` (`Option<Model>` / `Vec<Model>`).
fn relation_value(
    rel: &RelationFieldInfo,
    source_ident: &syn::Ident,
    source_module_path: &[String],
) -> TokenStream {
    let schema_path = &rel.schema_path;
    let single = matches!(rel.relation_type.as_str(), "HasOne" | "BelongsTo");
    let required = |value: TokenStream| {
        quote! {
            #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                format!("Required relation '{}' not found", stringify!(#source_ident))
            )).map(|r| #value)?
        }
    };
    if rel.adapter {
        // `relation_adapters` wrapper: `From<Option<Model>>`.
        return quote! { ::core::convert::From::from(#source_ident) };
    }
    if let Some((inline_type_name, included_fields)) = &rel.inline_type_info {
        // The related model (relations stripped) behind the inline type.
        let model_path = normalize_token_str(schema_path).replace("::Schema", "::Model");
        let related = get_struct_from_schema_path(&model_path);
        let related_def = related.as_ref().map_or("", |s| s.definition.as_str());
        let construct =
            generate_inline_type_construction(inline_type_name, included_fields, related_def, "r");
        return match rel.relation_type.as_str() {
            "HasMany" => quote! { #source_ident.into_iter().map(|r| #construct).collect() },
            _ if !single => quote! { Default::default() },
            _ if rel.is_optional => quote! { #source_ident.map(|r| Box::new(#construct)) },
            _ => required(quote! { Box::new(#construct) }),
        };
    }
    if !single {
        return if rel.relation_type == "HasMany" {
            quote! { #source_ident.into_iter().map(|r| <#schema_path as From<_>>::from(r)).collect() }
        } else {
            quote! { Default::default() }
        };
    }
    // Not circular (an inline type would exist): a target `Schema` that
    // keeps relation fields loads them through its async `from_model`.  The
    // `schema_type!` generating it decides (its `omit` / `pick` / `add`);
    // without one, a target `Model` with FK relations is taken to have them.
    let schema_str = normalize_token_str(schema_path);
    let model_path = schema_str.replace("::Schema", "::Model");
    let related = get_struct_from_schema_path(&model_path);
    let related_def = related.as_ref().map_or("", |s| s.definition.as_str());
    let related_module = get_module_path_from_schema_path(schema_path);
    let schema_name = schema_str.rsplit("::").next().unwrap_or_default().trim();
    let target_has_from_model = match target_shape(&related_module, schema_name, related_def) {
        Some(TargetShape::FromModel) => true,
        Some(TargetShape::From) => false,
        Some(TargetShape::Neither) => {
            let message = format!(
                "schema_type!: relation field `{source_ident}` converts its row into `{schema_str}`, but the `schema_type!` generating it uses `add` or `multipart`, which emit neither `From` nor `from_model`; `omit` the field or give it a `relation_adapters` wrapper"
            );
            return quote! { ::core::compile_error!(#message) };
        }
        None => {
            get_circular_analysis(source_module_path, &related_module, related_def)
                .has_fk_relations
        }
    };
    match (target_has_from_model, rel.is_optional) {
        (true, true) => quote! {
            match #source_ident {
                Some(r) => Some(Box::new(#schema_path::from_model(r, #DB).await?)),
                None => None,
            }
        },
        (true, false) => quote! {
            Box::new(#schema_path::from_model(
                #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                    format!("Required relation '{}' not found", stringify!(#source_ident))
                ))?,
                #DB,
            ).await?)
        },
        (false, true) => {
            quote! { #source_ident.map(|r| Box::new(<#schema_path as From<_>>::from(r))) }
        }
        (false, false) => required(quote! { Box::new(<#schema_path as From<_>>::from(r)) }),
    }
}

#[cfg(test)]
mod tests;
