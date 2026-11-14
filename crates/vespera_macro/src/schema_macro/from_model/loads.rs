//! Relation loading statements of the generated `from_model`.
//!
//! Every SeaORM trait method is called fully qualified
//! (`<E as sea_orm::EntityTrait>::find()`, `sea_orm::QueryFilter::filter`,
//! `sea_orm::ColumnTrait::eq`, `sea_orm::ModelTrait::find_related`), so the
//! generated code compiles without any `use sea_orm::...` at the call site.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::super::{
    file_cache::get_fk_column,
    seaorm::RelationFieldInfo,
    type_utils::{normalize_token_str, snake_to_pascal_case},
};
use super::build_entity_path_from_schema_path;

/// The `let <field> = ...;` statement loading one relation of `model`.
///
/// # Errors
///
/// A `#[sea_orm(...)]` value that must become an identifier but is not one,
/// and a relation whose rows cannot be selected: a `relation_enum` / `via_rel`
/// whose foreign key is not found on the target entity (previously an empty
/// `vec![]` / an uncompilable `Relation::X` query).
pub(super) fn relation_load(
    rel: &RelationFieldInfo,
    source_type: &Type,
    source_module_path: &[String],
) -> syn::Result<TokenStream> {
    let field_name = &rel.field_name;
    let entity_path = build_entity_path_from_schema_path(&rel.schema_path, source_module_path);
    let entity = quote! { <#entity_path as sea_orm::EntityTrait> };
    let ident = |key: &str, value: &str| sea_orm_value_ident(source_type, field_name, key, value);
    let single = match rel.relation_type.as_str() {
        "HasOne" | "BelongsTo" => true,
        "HasMany" => false,
        _ => return Ok(TokenStream::new()),
    };

    if single && let Some(fk_col) = &rel.fk_column {
        // The FK lives on the source model: select the target by its key.
        let fk_ident = ident("from", fk_col)?;
        return Ok(if rel.is_optional {
            quote! {
                let #field_name = match &model.#fk_ident {
                    Some(fk_value) => #entity::find_by_id(fk_value.clone()).one(db).await?,
                    None => None,
                };
            }
        } else {
            quote! {
                let #field_name = #entity::find_by_id(model.#fk_ident.clone()).one(db).await?;
            }
        });
    }

    // Named relation without a source-side FK: the FK lives on the target
    // entity's relation of that name (`via_rel`, else `relation_enum`).
    let Some(fk_rel) = rel.via_rel.as_ref().or(rel.relation_enum.as_ref()) else {
        let fetch = if single { quote!(one) } else { quote!(all) };
        return Ok(quote! {
            let #field_name = sea_orm::ModelTrait::find_related(&model, #entity_path).#fetch(db).await?;
        });
    };
    let schema_path_str = normalize_token_str(&rel.schema_path);
    let Some((from, to)) = get_fk_column(&schema_path_str, fk_rel) else {
        return Err(syn::Error::new_spanned(
            source_type,
            format!(
                "schema_type!: cannot load relation field `{field_name}`: no field of the `Model` behind `{schema_path_str}` has `#[sea_orm(relation_enum = \"{fk_rel}\", from = \"..\")]`; add it there, or `omit` the field"
            ),
        ));
    };
    let from_ident = ident("from", &from)
        .and_then(|_| ident("from", &snake_to_pascal_case(&from)))
        .map_err(|_| {
            syn::Error::new_spanned(
                source_type,
                format!(
                    "schema_type!: relation field `{field_name}` resolves its foreign key through `{fk_rel}` to `#[sea_orm(from = \"{from}\")]` on `{schema_path_str}`, but `{from}` is not a valid Rust identifier"
                ),
            )
        })?;
    let to_ident = ident("to", to.as_deref().unwrap_or("id"))?;
    let column = column_path(&entity_path);
    let fetch = if single { quote!(one) } else { quote!(all) };
    Ok(quote! {
        let #field_name = sea_orm::QueryFilter::filter(
            #entity::find(),
            sea_orm::ColumnTrait::eq(&#column::#from_ident, model.#to_ident.clone()),
        )
        .#fetch(db)
        .await?;
    })
}

/// `path::to::Entity` → `path::to::Column`.
fn column_path(entity_path: &TokenStream) -> TokenStream {
    let mut path: syn::Path =
        syn::parse2(entity_path.clone()).unwrap_or_else(|_| syn::parse_quote!(Entity));
    if let Some(last) = path.segments.last_mut() {
        last.ident = syn::Ident::new("Column", last.ident.span());
    }
    quote! { #path }
}

/// Parses a `#[sea_orm(key = "value")]` string as an identifier (keywords
/// accepted, like the `syn::Ident::new` it replaces), or a `syn::Error`
/// spanned on the `schema_type!` source type naming the relation field.
pub(super) fn sea_orm_value_ident(
    source_type: &Type,
    field_name: &syn::Ident,
    key: &str,
    value: &str,
) -> syn::Result<syn::Ident> {
    syn::parse::Parser::parse_str(syn::ext::IdentExt::parse_any, value).map_err(|_| {
        syn::Error::new_spanned(
            source_type,
            format!(
                "schema_type!: relation field `{field_name}` has `#[sea_orm({key} = \"{value}\")]`, but `{value}` is not a valid Rust identifier"
            ),
        )
    })
}
