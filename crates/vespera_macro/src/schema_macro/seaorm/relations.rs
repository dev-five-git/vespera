use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::attrs::{extract_belongs_to_from_field, extract_relation_enum, extract_via_rel};
use crate::schema_macro::type_utils::is_option_type;

/// Relation field info for generating `from_model` code.
#[derive(Clone)]
pub struct RelationFieldInfo {
    pub field_name: syn::Ident,
    pub relation_type: String,
    pub schema_path: TokenStream,
    pub is_optional: bool,
    pub inline_type_info: Option<(syn::Ident, Vec<String>)>,
    pub relation_enum: Option<String>,
    pub fk_column: Option<String>,
    pub via_rel: Option<String>,
}

/// Check if a field in the struct is optional (`Option<T>`).
pub fn is_field_optional_in_struct(struct_item: &syn::ItemStruct, field_name: &str) -> bool {
    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            if let Some(ident) = &field.ident
                && ident == field_name
            {
                return is_option_type(&field.ty);
            }
        }
    }
    false
}

/// Convert a `SeaORM` relation type to a Schema type AND return relation info.
pub fn convert_relation_type_to_schema_with_info(
    ty: &Type,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    source_module_path: &[String],
    field_name: syn::Ident,
) -> Option<(TokenStream, RelationFieldInfo)> {
    let Type::Path(type_path) = ty else {
        return None;
    };

    let segment = type_path.path.segments.last()?;
    let ident_str = segment.ident.to_string();
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    let Type::Path(inner_path) = inner_ty else {
        return None;
    };

    let schema_path = schema_path_tokens(&inner_path.path, source_module_path);

    match ident_str.as_str() {
        "HasOne" => Some(single_relation(
            "HasOne",
            field_name,
            field_attrs,
            parsed_struct,
            schema_path,
        )),
        "HasMany" => {
            let relation_enum = extract_relation_enum(field_attrs);
            let via_rel = extract_via_rel(field_attrs);
            let converted = quote! { Vec<#schema_path> };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasMany".to_string(),
                schema_path,
                is_optional: false,
                inline_type_info: None,
                relation_enum,
                fk_column: None,
                via_rel,
            };
            Some((converted, info))
        }
        "BelongsTo" => Some(single_relation(
            "BelongsTo",
            field_name,
            field_attrs,
            parsed_struct,
            schema_path,
        )),
        _ => None,
    }
}

fn schema_path_tokens(path: &syn::Path, source_module_path: &[String]) -> TokenStream {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let absolute_segments = absolute_schema_segments(&segments, source_module_path);
    let path_idents: Vec<syn::Ident> = absolute_segments
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    quote! { #(#path_idents)::* }
}

fn absolute_schema_segments(segments: &[String], source_module_path: &[String]) -> Vec<String> {
    if !segments.is_empty() && segments[0] == "super" {
        let super_count = segments.iter().take_while(|s| *s == "super").count();
        let parent_path_len = source_module_path.len().saturating_sub(super_count);
        let mut abs = Vec::with_capacity(parent_path_len + segments.len() - super_count);
        abs.extend_from_slice(&source_module_path[..parent_path_len]);
        abs.extend(segments.iter().skip(super_count).map(entity_to_schema));
        abs
    } else if !segments.is_empty() && segments[0] == "crate" {
        segments.iter().map(entity_to_schema).collect()
    } else {
        let parent_path_len = source_module_path.len().saturating_sub(1);
        let mut abs = Vec::with_capacity(parent_path_len + segments.len());
        abs.extend_from_slice(&source_module_path[..parent_path_len]);
        abs.extend(segments.iter().map(entity_to_schema));
        abs
    }
}

fn entity_to_schema(segment: &String) -> String {
    if segment == "Entity" {
        "Schema".to_string()
    } else {
        segment.clone()
    }
}

fn single_relation(
    relation_type: &str,
    field_name: syn::Ident,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    schema_path: TokenStream,
) -> (TokenStream, RelationFieldInfo) {
    let fk_field = extract_belongs_to_from_field(field_attrs);
    let relation_enum = extract_relation_enum(field_attrs);
    let is_optional = fk_field
        .as_ref()
        .is_none_or(|f| is_field_optional_in_struct(parsed_struct, f));

    let converted = if is_optional {
        quote! { Option<Box<#schema_path>> }
    } else {
        quote! { Box<#schema_path> }
    };
    let info = RelationFieldInfo {
        field_name,
        relation_type: relation_type.to_string(),
        schema_path,
        is_optional,
        inline_type_info: None,
        relation_enum,
        fk_column: fk_field,
        via_rel: None,
    };
    (converted, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_struct(def: &str) -> syn::ItemStruct {
        syn::parse_str(def).unwrap()
    }

    fn ident(name: &str) -> syn::Ident {
        syn::Ident::new(name, proc_macro2::Span::call_site())
    }

    #[test]
    fn test_is_field_optional_in_struct_optional() {
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        assert!(is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_required() {
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        assert!(!is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_field_not_found() {
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(!is_field_optional_in_struct(&struct_item, "nonexistent"));
    }

    #[test]
    fn test_is_field_optional_in_struct_tuple_struct() {
        let struct_item: syn::ItemStruct =
            syn::parse_str("struct TupleStruct(i32, Option<String>);").unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "0"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_empty_segments() {
        let ty = syn::Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_no_angle_brackets() {
        let ty: syn::Type = syn::parse_str("HasOne").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_type_generic() {
        let ty: syn::Type = syn::parse_str("HasOne<'a>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_inner() {
        let ty: syn::Type = syn::parse_str("HasOne<&str>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_required() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Box"));
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_no_fk() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasMany");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Vec"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(!info.is_optional);
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_unknown_relation() {
        let ty: syn::Type = syn::parse_str("SomeOtherType<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_super_path() {
        let ty: syn::Type = syn::parse_str("HasMany<super::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("memo"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_crate_path() {
        let ty: syn::Type = syn::parse_str("HasMany<crate::models::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("memo"));
        assert!(output.contains("Schema"));
        assert!(!output.contains("Entity"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_relative_path() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("user"));
        assert!(output.contains("Schema"));
    }
}
