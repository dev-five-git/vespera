//! Relation loading statements of the generated `from_model`.
//!
//! Every SeaORM trait method is called fully qualified
//! (`<E as sea_orm::EntityTrait>::find()`, `sea_orm::QueryFilter::filter`,
//! `sea_orm::ColumnTrait::eq`, `sea_orm::ModelTrait::find_related`), so the
//! generated code compiles without any `use sea_orm::...` at the call site.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Type, ext::IdentExt};

use super::super::{
    file_cache::{get_fk_column, get_struct_from_schema_path},
    seaorm::{RelationFieldInfo, is_composite_columns, is_sea_orm_primary_key, normalize_columns},
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
    source_fields: &syn::Fields,
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
                let to_ident = ident("to", to).and_then(|_| ident("to", &column_variant(to)))?;
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
        .and_then(|_| ident("from", &column_variant(&from)))
        .map_err(|_| {
            syn::Error::new_spanned(
                source_type,
                format!(
                    "schema_type!: relation field `{field_name}` resolves its foreign key through `{fk_rel}` to `#[sea_orm(from = \"{from}\")]` on `{schema_path_str}`, but `{from}` is not a valid Rust identifier"
                ),
            )
        })?;
    // `to` names a column of the SOURCE model (`Column::Address2Id` is its
    // `address_2_id` field), so it is normalized against the source fields.
    let to = to.map_or_else(
        || source_key_column(source_fields),
        |to| normalize_columns(&to, source_fields),
    );
    let to_ident = ident("to", &to)?;
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

/// The source-model field the generated load of `rel` reads (R21-SM2-L1,
/// round 21): the `from` column of a single relation whose key is on the
/// source, else the source column its target-side relation names (`to`, or
/// the source key). `None` when the load reads no source field. The DTO
/// module must be able to read it, whether or not the DTO emits it.
pub(in crate::schema_macro) fn source_column_read(
    rel: &RelationFieldInfo,
    source_fields: &syn::Fields,
) -> Option<String> {
    let single = match rel.relation_type.as_str() {
        "HasOne" | "BelongsTo" => true,
        "HasMany" => false,
        _ => return None,
    };
    if single && let Some(fk_col) = &rel.fk_column {
        return Some(fk_col.clone());
    }
    let fk_rel = rel.via_rel.as_ref().or(rel.relation_enum.as_ref())?;
    let (_, to) = get_fk_column(&normalize_token_str(&rel.schema_path), fk_rel)?;
    Some(to.map_or_else(
        || source_key_column(source_fields),
        |to| normalize_columns(&to, source_fields),
    ))
}

/// The source field named `column` (a raw field is named by its column).
pub(in crate::schema_macro) fn source_field_named<'a>(
    source_fields: &'a syn::Fields,
    column: &str,
) -> Option<&'a syn::Field> {
    source_fields.iter().find(|field| {
        field
            .ident
            .as_ref()
            .is_some_and(|ident| ident.unraw() == column)
    })
}

/// The source column a `belongs_to` without `to` references: SeaORM's default
/// is the referenced entity's primary key, and here that entity is the source
/// model, so its single `#[sea_orm(primary_key)]` field (R21-SM2-M1, round 21).
/// `id` when the source declares no single primary key, as before.
fn source_key_column(source_fields: &syn::Fields) -> String {
    let mut keys = source_fields
        .iter()
        .filter(|field| is_sea_orm_primary_key(&field.attrs));
    match (keys.next(), keys.next()) {
        (Some(key), None) => key
            .ident
            .as_ref()
            .map_or_else(|| "id".to_string(), |ident| ident.unraw().to_string()),
        _ => "id".to_string(),
    }
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

/// The `Column` variant of a SeaORM column value: `address_id` → `AddressId`,
/// and a raw field `r#type` (SeaORM's `Column::Type`) → `Type`. The `r#`
/// prefix is stripped first: converting it as text made `R#type`.
fn column_variant(value: &str) -> String {
    snake_to_pascal_case(value.trim_start_matches("r#"))
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

/// Parses a `#[sea_orm(key = "value")]` string as an identifier, or a
/// `syn::Error` spanned on the `schema_type!` source type naming the relation
/// field. A keyword column (`from = "ref"`) is the raw field `r#ref`: a plain
/// `ref` ident made `__vespera_model.ref`, which does not parse.
pub(super) fn sea_orm_value_ident(
    source_type: &Type,
    field_name: &syn::Ident,
    key: &str,
    value: &str,
) -> syn::Result<syn::Ident> {
    syn::parse::Parser::parse_str(syn::ext::IdentExt::parse_any, value).map(raw_if_keyword).map_err(|_| {
        syn::Error::new_spanned(
            source_type,
            format!(
                "schema_type!: relation field `{field_name}` has `#[sea_orm({key} = \"{value}\")]`, but `{value}` is not a valid Rust identifier"
            ),
        )
    })
}

/// `ident` as a raw identifier when it is a keyword that can be one
/// (`ref` → `r#ref`; `self` / `Self` / `super` / `crate` cannot).
fn raw_if_keyword(ident: syn::Ident) -> syn::Ident {
    let name = ident.to_string();
    if syn::parse_str::<syn::Ident>(&name).is_ok()
        || matches!(name.as_str(), "self" | "Self" | "super" | "crate")
    {
        ident
    } else {
        syn::Ident::new_raw(&name, ident.span())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::plain("user_id", "user_id")]
    #[case::keyword("ref", "r#ref")]
    #[case::already_raw("r#type", "r#type")]
    #[case::pascal("Ref", "Ref")]
    #[case::not_rawable("Self", "Self")]
    fn keyword_columns_become_raw_fields(#[case] value: &str, #[case] expected: &str) {
        let source: Type = syn::parse_quote!(Model);
        let field = syn::Ident::new("parent", proc_macro2::Span::call_site());
        let ident = sea_orm_value_ident(&source, &field, "from", value).unwrap();
        assert_eq!(ident.to_string(), expected);
    }

    /// A raw-identifier column (`from = "r#type"`) names the `Column::Type`
    /// variant: the variant must be a valid identifier.
    #[rstest::rstest]
    #[case::snake("address_id", "AddressId")]
    #[case::keyword("ref", "Ref")]
    #[case::raw_field("r#type", "Type")]
    #[case::raw_snake("r#user_id", "UserId")]
    fn column_variants_are_valid_identifiers(#[case] value: &str, #[case] expected: &str) {
        let source: Type = syn::parse_quote!(Model);
        let field = syn::Ident::new("parent", proc_macro2::Span::call_site());
        let variant = column_variant(value);
        assert_eq!(variant, expected);
        assert!(sea_orm_value_ident(&source, &field, "from", &variant).is_ok());
    }

    /// The source fields of `pub struct Model { .. }`.
    fn source_fields(body: &str) -> syn::Fields {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("pub struct Model {{ {body} }}")).unwrap();
        item.fields
    }

    /// `notifications` reached through `Owner`: the target `notification`
    /// holds the FK `owner_id` and no `to`, so the load reads the source key.
    fn notifications_rel() -> RelationFieldInfo {
        RelationFieldInfo {
            field_name: syn::Ident::new("notifications", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote::quote! { crate::models::notification::Schema },
            is_optional: false,
            inline_type_info: None,
            relation_enum: Some("Owner".to_string()),
            fk_column: None,
            fk_optional: false,
            fk_to: None,
            via_rel: None,
            adapter: false,
        }
    }

    /// The `notification` model: `owner` is the `Owner` relation of
    /// `super::user::Entity` with no `to` (the source key is the reference).
    const NOTIFICATION: &str = "pub struct Model { pub id: i32, pub owner_id: i32, #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_id\", relation_enum = \"Owner\")] pub owner: BelongsTo<super::user::Entity> }";

    /// The same relation with an explicit `to = "code"` (a source column).
    const NOTIFICATION_TO_CODE: &str = "pub struct Model { pub id: i32, pub owner_code: String, #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_code\", to = \"code\", relation_enum = \"Owner\")] pub owner: BelongsTo<super::user::Entity> }";

    /// Runs `check` in a crate whose `notification` model is `model`.
    fn with_notification_model(model: &str, check: impl FnOnce()) {
        let dir = tempfile::TempDir::new().unwrap();
        let models = dir.path().join("src").join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("notification.rs"), model).unwrap();
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        crate::schema_macro::file_cache::bump_epoch();
        check();
    }

    /// A relation with `to` names that source column, not the key.
    #[test]
    fn source_column_read_follows_an_explicit_to() {
        let fields = source_fields("pub code: String");
        with_notification_model(NOTIFICATION_TO_CODE, || {
            assert_eq!(
                source_column_read(&notifications_rel(), &fields).as_deref(),
                Some("code")
            );
        });
    }

    // R21-SM2-M1 (round 21): without `to` the source key is the source's own
    // primary key, not a literal `id`.
    #[rstest::rstest]
    #[case::declared_key(
        "#[sea_orm(primary_key)] pub user_id: i32, pub name: String",
        "user_id"
    )]
    #[case::no_key_falls_back_to_id("pub id: i32, pub name: String", "id")]
    #[case::two_keys_fall_back_to_id(
        "#[sea_orm(primary_key)] pub a: i32, #[sea_orm(primary_key)] pub b: i32",
        "id"
    )]
    fn load_without_to_reads_the_source_key(#[case] body: &str, #[case] expected: &str) {
        let fields = source_fields(body);
        with_notification_model(NOTIFICATION, || {
            assert_eq!(
                source_column_read(&notifications_rel(), &fields).as_deref(),
                Some(expected)
            );
            let source: Type = syn::parse_quote!(crate::models::user::Model);
            let module = ["crate", "models", "user"].map(String::from);
            let load = relation_load(&notifications_rel(), &source, &module, &fields)
                .unwrap()
                .to_string();
            assert!(load.contains(&format!(". {expected}")), "{load}");
        });
    }

    // R21-SM2-L1 (round 21): a relation load reads its source column only when
    // it has one; a relation kind it does not load reads none.
    #[test]
    fn source_column_read_follows_the_load() {
        let fields = source_fields("pub user_id: i32");
        let mut fk_on_source = notifications_rel();
        fk_on_source.relation_type = "BelongsTo".to_string();
        fk_on_source.fk_column = Some("user_id".to_string());
        assert_eq!(
            source_column_read(&fk_on_source, &fields).as_deref(),
            Some("user_id")
        );
        let mut other = notifications_rel();
        other.relation_type = "Unknown".to_string();
        assert_eq!(source_column_read(&other, &fields), None);
    }

    #[test]
    fn source_field_named_matches_raw_fields_by_column() {
        let fields = source_fields("pub r#type: String, pub id: i32");
        assert!(source_field_named(&fields, "type").is_some());
        assert!(source_field_named(&fields, "id").is_some());
        assert!(source_field_named(&fields, "missing").is_none());
    }
}
