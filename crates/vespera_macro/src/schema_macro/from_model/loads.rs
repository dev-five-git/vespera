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
    file_cache::{get_fk_column, get_struct_from_schema_path},
    seaorm::{RelationFieldInfo, is_composite_columns, is_sea_orm_primary_key},
    type_utils::{normalize_token_str, snake_to_pascal_case},
};
use super::{DB, MODEL, build_entity_path_from_schema_path};

/// The `let <field> = ...;` statement loading one relation of the model.
///
/// # Errors
///
/// A `#[sea_orm(...)]` value that must become an identifier but is not one,
/// and a relation whose rows cannot be selected: a `relation_enum` / `via_rel`
/// whose foreign key is not found on the target entity (previously an empty
/// `vec![]` / an uncompilable `Relation::X` query), and a composite
/// `from = "(a, b)"` key.
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
    let schema_path_str = normalize_token_str(&rel.schema_path);

    if single && let Some(fk_col) = &rel.fk_column {
        // The FK lives on the source model: select the target by the
        // referenced column (`find_by_id` when it is the primary key).
        for (key, columns) in [("from", Some(fk_col)), ("to", rel.fk_to.as_ref())] {
            if let Some(columns) = columns.filter(|c| is_composite_columns(c)) {
                return Err(syn::Error::new_spanned(
                    source_type,
                    format!(
                        "schema_type!: relation field `{field_name}` has a composite key `#[sea_orm({key} = \"{columns}\")]`, which `from_model` cannot load; `omit` the field"
                    ),
                ));
            }
        }
        let fk_ident = ident("from", fk_col)?;
        let select = match rel.fk_to.as_deref() {
            Some(to) if !is_target_primary_key(&schema_path_str, to) => {
                let column = column_path(&entity_path);
                // The raw value first: the diagnostic quotes what was written.
                let to_ident =
                    ident("to", to).and_then(|_| ident("to", &snake_to_pascal_case(to)))?;
                quote! {
                    sea_orm::QueryFilter::filter(
                        #entity::find(),
                        sea_orm::ColumnTrait::eq(&#column::#to_ident, fk_value.clone()),
                    )
                }
            }
            _ => quote! { #entity::find_by_id(fk_value.clone()) },
        };
        return Ok(if rel.fk_optional {
            quote! {
                let #field_name = match &#MODEL.#fk_ident {
                    Some(fk_value) => #select.one(#DB).await?,
                    None => None,
                };
            }
        } else {
            quote! {
                let #field_name = {
                    let fk_value = &#MODEL.#fk_ident;
                    #select.one(#DB).await?
                };
            }
        });
    }

    // Named relation without a source-side FK: the FK lives on the target
    // entity's relation of that name (`via_rel`, else `relation_enum`).
    let fetch = if single { quote!(one) } else { quote!(all) };
    let Some(fk_rel) = rel.via_rel.as_ref().or(rel.relation_enum.as_ref()) else {
        return Ok(quote! {
            let #field_name = sea_orm::ModelTrait::find_related(&#MODEL, #entity_path).#fetch(#DB).await?;
        });
    };
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
    Ok(quote! {
        let #field_name = sea_orm::QueryFilter::filter(
            #entity::find(),
            sea_orm::ColumnTrait::eq(&#column::#from_ident, #MODEL.#to_ident.clone()),
        )
        .#fetch(#DB)
        .await?;
    })
}

/// Whether `column` is the whole primary key of the `Model` behind
/// `schema_path` (`find_by_id` selects by it). Without the model on disk,
/// SeaORM's conventional `id` is assumed.
fn is_target_primary_key(schema_path: &str, column: &str) -> bool {
    let model_path = super::model_path_of(schema_path);
    // A stored definition always parses.
    let Some(parsed) = get_struct_from_schema_path(&model_path)
        .and_then(|model| super::super::file_cache::parse_struct_cached(&model.definition).ok())
    else {
        return column == "id";
    };
    let keys: Vec<String> = parsed
        .fields
        .iter()
        .filter(|field| is_sea_orm_primary_key(&field.attrs))
        .filter_map(|field| field.ident.as_ref())
        .map(|ident| crate::parser::strip_raw_prefix_owned(ident.to_string()))
        .collect();
    keys == [column]
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
