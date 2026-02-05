//! SeaORM and Chrono type conversions
//!
//! Handles conversion of SeaORM relation types and datetime types to their
//! schema equivalents.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::type_utils::{is_option_type, resolve_type_to_absolute_path};

/// Relation field info for generating from_model code
#[derive(Clone)]
pub struct RelationFieldInfo {
    /// Field name in the generated struct
    pub field_name: syn::Ident,
    /// Relation type: "HasOne", "HasMany", or "BelongsTo"
    pub relation_type: String,
    /// Target Schema path (e.g., crate::models::user::Schema)
    pub schema_path: TokenStream,
    /// Whether the relation is optional
    pub is_optional: bool,
    /// If Some, this relation has circular refs and uses an inline type
    /// Contains: (inline_type_name, circular_fields_to_exclude)
    pub inline_type_info: Option<(syn::Ident, Vec<String>)>,
    /// The `relation_enum` attribute value (e.g., "TargetUser", "CreatedByUser")
    /// When present, indicates multiple relations to the same Entity type exist
    pub relation_enum: Option<String>,
    /// The FK column name from `from` attribute (e.g., "user_id", "target_user_id")
    pub fk_column: Option<String>,
    /// The `via_rel` attribute value for HasMany relations (e.g., "TargetUser")
    /// This specifies which Relation variant on the TARGET entity to use
    pub via_rel: Option<String>,
}

/// Convert SeaORM datetime types to chrono equivalents.
///
/// This allows generated schemas to use standard chrono types instead of
/// requiring `use sea_orm::entity::prelude::DateTimeWithTimeZone`.
///
/// Conversions:
/// - `DateTimeWithTimeZone` -> `chrono::DateTime<chrono::FixedOffset>`
/// - `DateTimeUtc` -> `chrono::DateTime<chrono::Utc>`
/// - `DateTimeLocal` -> `chrono::DateTime<chrono::Local>`
/// - `DateTime` (SeaORM) -> `chrono::NaiveDateTime`
/// - `Date` (SeaORM) -> `chrono::NaiveDate`
/// - `Time` (SeaORM) -> `chrono::NaiveTime`
///
/// Returns the original type as TokenStream if not a SeaORM datetime type.
pub fn convert_seaorm_type_to_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return quote! { #ty },
    };

    let segment = match type_path.path.segments.last() {
        Some(s) => s,
        None => return quote! { #ty },
    };

    let ident_str = segment.ident.to_string();

    match ident_str.as_str() {
        // Use vespera::chrono to avoid requiring users to add chrono dependency
        "DateTimeWithTimeZone" => {
            quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
        }
        "DateTimeUtc" => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
        "DateTimeLocal" => quote! { vespera::chrono::DateTime<vespera::chrono::Local> },
        // Not a SeaORM datetime type - resolve to absolute path if needed
        _ => resolve_type_to_absolute_path(ty, source_module_path),
    }
}

/// Convert a type to chrono equivalent, handling Option<T> wrapper.
///
/// If the type is `Option<SeaOrmType>`, converts to `Option<ChronoType>`.
/// If the type is just `SeaOrmType`, converts to `ChronoType`.
///
/// Also resolves local types (like `MemoStatus`) to absolute paths
/// (like `crate::models::memo::MemoStatus`) using source_module_path.
pub fn convert_type_with_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    // Check if it's Option<T>
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.first()
        && segment.ident == "Option"
    {
        // Extract the inner type from Option<T>
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
        {
            let converted_inner = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
            return quote! { Option<#converted_inner> };
        }
    }

    // Check if it's Vec<T>
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.first()
        && segment.ident == "Vec"
    {
        // Extract the inner type from Vec<T>
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
        {
            let converted_inner = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
            return quote! { Vec<#converted_inner> };
        }
    }

    // Not Option or Vec, convert directly
    convert_seaorm_type_to_chrono(ty, source_module_path)
}

/// Extract the "from" field name from a sea_orm belongs_to attribute.
/// e.g., `#[sea_orm(belongs_to, from = "user_id", to = "id")]` -> Some("user_id")
/// Also handles: `#[sea_orm(belongs_to = "Entity", from = "user_id", to = "id")]`
pub fn extract_belongs_to_from_field(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("sea_orm") {
            return None;
        }

        let mut from_field = None;
        // Ignore parse errors - we just won't find the field if parsing fails
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("from") {
                from_field = meta
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<syn::LitStr>().ok())
                    .map(|lit| lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Consume value for key=value pairs (e.g., belongs_to = "...", to = "...")
                // Required to allow parsing to continue to next item
                drop(meta.value().and_then(|v| v.parse::<syn::LitStr>()));
            }
            Ok(())
        });
        from_field
    })
}

/// Extract the "relation_enum" value from a sea_orm attribute.
/// e.g., `#[sea_orm(belongs_to, relation_enum = "TargetUser", from = "target_user_id")]` -> Some("TargetUser")
///
/// When relation_enum is present, it indicates that multiple relations to the same
/// Entity type exist, and we need to use the specific Relation enum variant for queries.
pub fn extract_relation_enum(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("sea_orm") {
            return None;
        }

        let mut relation_enum_value = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("relation_enum") {
                relation_enum_value = meta
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<syn::LitStr>().ok())
                    .map(|lit| lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Consume value for other key=value pairs
                drop(meta.value().and_then(|v| v.parse::<syn::LitStr>()));
            }
            Ok(())
        });
        relation_enum_value
    })
}

/// Extract the "via_rel" value from a sea_orm attribute.
/// e.g., `#[sea_orm(has_many, relation_enum = "TargetUser", via_rel = "TargetUser")]` -> Some("TargetUser")
///
/// For HasMany relations with relation_enum, via_rel specifies which Relation variant
/// on the TARGET entity corresponds to this relation. This allows us to find the FK column.
pub fn extract_via_rel(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("sea_orm") {
            return None;
        }

        let mut via_rel_value = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("via_rel") {
                via_rel_value = meta
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<syn::LitStr>().ok())
                    .map(|lit| lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Consume value for other key=value pairs
                drop(meta.value().and_then(|v| v.parse::<syn::LitStr>()));
            }
            Ok(())
        });
        via_rel_value
    })
}

/// Check if a field in the struct is optional (Option<T>).
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

/// Convert a SeaORM relation type to a Schema type AND return relation info.
///
/// - `#[sea_orm(has_one)]` -> Always `Option<Box<Schema>>`
/// - `#[sea_orm(has_many)]` -> Always `Vec<Schema>`
/// - `#[sea_orm(belongs_to, from = "field")]`:
///   - If `from` field is `Option<T>` -> `Option<Box<Schema>>`
///   - If `from` field is required -> `Box<Schema>`
///
/// The `source_module_path` is used to resolve relative paths like `super::`.
/// e.g., if source is `crate::models::memo::Model`, module path is `crate::models::memo`
///
/// Returns None if the type is not a relation type or conversion fails.
/// Returns (TokenStream, RelationFieldInfo) on success for use in from_model generation.
pub fn convert_relation_type_to_schema_with_info(
    ty: &Type,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    source_module_path: &[String],
    field_name: syn::Ident,
) -> Option<(TokenStream, RelationFieldInfo)> {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    let segment = type_path.path.segments.last()?;
    let ident_str = segment.ident.to_string();

    // Check if this is a relation type with generic argument
    let args = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args,
        _ => return None,
    };

    // Get the inner Entity type
    let inner_ty = match args.args.first()? {
        syn::GenericArgument::Type(ty) => ty,
        _ => return None,
    };

    // Extract the path and convert to absolute Schema path
    let inner_path = match inner_ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    // Collect segments as strings
    let segments: Vec<String> = inner_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    // Convert path to absolute, resolving `super::` relative to source module
    let absolute_segments: Vec<String> = if !segments.is_empty() && segments[0] == "super" {
        let super_count = segments.iter().take_while(|s| *s == "super").count();
        let parent_path_len = source_module_path.len().saturating_sub(super_count);
        let mut abs = source_module_path[..parent_path_len].to_vec();
        for seg in segments.iter().skip(super_count) {
            if seg == "Entity" {
                abs.push("Schema".to_string());
            } else {
                abs.push(seg.clone());
            }
        }
        abs
    } else if !segments.is_empty() && segments[0] == "crate" {
        segments
            .iter()
            .map(|s| {
                if s == "Entity" {
                    "Schema".to_string()
                } else {
                    s.clone()
                }
            })
            .collect()
    } else {
        let parent_path_len = source_module_path.len().saturating_sub(1);
        let mut abs = source_module_path[..parent_path_len].to_vec();
        for seg in &segments {
            if seg == "Entity" {
                abs.push("Schema".to_string());
            } else {
                abs.push(seg.clone());
            }
        }
        abs
    };

    // Build the absolute path as tokens
    let path_idents: Vec<syn::Ident> = absolute_segments
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    let schema_path = quote! { #(#path_idents)::* };

    // Convert based on relation type
    match ident_str.as_str() {
        "HasOne" => {
            // HasOne -> Check FK field to determine optionality
            // If FK is Option<T> -> relation is optional: Option<Box<Schema>>
            // If FK is required -> relation is required: Box<Schema>
            let fk_field = extract_belongs_to_from_field(field_attrs);
            let relation_enum = extract_relation_enum(field_attrs);
            let is_optional = fk_field
                .as_ref()
                .map(|f| is_field_optional_in_struct(parsed_struct, f))
                .unwrap_or(true); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasOne".to_string(),
                schema_path: schema_path.clone(),
                is_optional,
                inline_type_info: None, // Will be populated later if circular
                relation_enum,
                fk_column: fk_field,
                via_rel: None, // Not used for HasOne
            };
            Some((converted, info))
        }
        "HasMany" => {
            let relation_enum = extract_relation_enum(field_attrs);
            let via_rel = extract_via_rel(field_attrs);
            let converted = quote! { Vec<#schema_path> };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasMany".to_string(),
                schema_path: schema_path.clone(),
                is_optional: false,
                inline_type_info: None, // Will be populated later if circular
                relation_enum,
                fk_column: None, // HasMany doesn't have FK on this side
                via_rel,         // Used to find FK on target entity
            };
            Some((converted, info))
        }
        "BelongsTo" => {
            // BelongsTo -> Check FK field to determine optionality
            // If FK is Option<T> -> relation is optional: Option<Box<Schema>>
            // If FK is required -> relation is required: Box<Schema>
            let fk_field = extract_belongs_to_from_field(field_attrs);
            let relation_enum = extract_relation_enum(field_attrs);
            let is_optional = fk_field
                .as_ref()
                .map(|f| is_field_optional_in_struct(parsed_struct, f))
                .unwrap_or(true); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "BelongsTo".to_string(),
                schema_path: schema_path.clone(),
                is_optional,
                inline_type_info: None, // Will be populated later if circular
                relation_enum,
                fk_column: fk_field,
                via_rel: None, // Not used for BelongsTo
            };
            Some((converted, info))
        }
        _ => None,
    }
}

/// Convert a SeaORM relation type to a Schema type.
///
/// - `#[sea_orm(has_one)]` -> Always `Option<Box<Schema>>`
/// - `#[sea_orm(has_many)]` -> Always `Vec<Schema>`
/// - `#[sea_orm(belongs_to, from = "field")]`:
///   - If `from` field is `Option<T>` -> `Option<Box<Schema>>`
///   - If `from` field is required -> `Box<Schema>`
///
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(
        "DateTimeWithTimeZone",
        "vespera :: chrono :: DateTime < vespera :: chrono :: FixedOffset >"
    )]
    #[case(
        "DateTimeUtc",
        "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
    )]
    #[case(
        "DateTimeLocal",
        "vespera :: chrono :: DateTime < vespera :: chrono :: Local >"
    )]
    fn test_convert_seaorm_type_to_chrono(#[case] input: &str, #[case] expected_contains: &str) {
        let ty: syn::Type = syn::parse_str(input).unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(output.contains(expected_contains));
    }

    #[test]
    fn test_convert_seaorm_type_to_chrono_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(output.contains("& str"));
    }

    #[test]
    fn test_convert_seaorm_type_to_chrono_regular_type() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert_eq!(output.trim(), "String");
    }

    #[test]
    fn test_convert_type_with_chrono_option_datetime() {
        let ty: syn::Type = syn::parse_str("Option<DateTimeWithTimeZone>").unwrap();
        let tokens = convert_type_with_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(output.contains("Option <"));
        assert!(output.contains("vespera :: chrono :: DateTime"));
    }

    #[test]
    fn test_convert_type_with_chrono_vec_datetime() {
        let ty: syn::Type = syn::parse_str("Vec<DateTimeWithTimeZone>").unwrap();
        let tokens = convert_type_with_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(output.contains("Vec <"));
        assert!(output.contains("vespera :: chrono :: DateTime"));
    }

    #[test]
    fn test_convert_type_with_chrono_plain_type() {
        let ty: syn::Type = syn::parse_str("i32").unwrap();
        let tokens = convert_type_with_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert_eq!(output.trim(), "i32");
    }

    #[test]
    fn test_extract_belongs_to_from_field_with_from() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to, from = "user_id", to = "id")]
        )];
        let result = extract_belongs_to_from_field(&attrs);
        assert_eq!(result, Some("user_id".to_string()));
    }

    #[test]
    fn test_extract_belongs_to_from_field_without_from() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to, to = "id")]
        )];
        let result = extract_belongs_to_from_field(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_belongs_to_from_field_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        let result = extract_belongs_to_from_field(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_belongs_to_from_field_empty_attrs() {
        let result = extract_belongs_to_from_field(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_relation_enum_with_value() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to, relation_enum = "TargetUser", from = "target_user_id", to = "id")]
        )];
        let result = extract_relation_enum(&attrs);
        assert_eq!(result, Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_relation_enum_without_relation_enum() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to, from = "user_id", to = "id")]
        )];
        let result = extract_relation_enum(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_relation_enum_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        let result = extract_relation_enum(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_relation_enum_empty_attrs() {
        let result = extract_relation_enum(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_is_field_optional_in_struct_optional() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Model {
                id: i32,
                user_id: Option<i32>,
            }
        "#,
        )
        .unwrap();
        assert!(is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_required() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Model {
                id: i32,
                user_id: i32,
            }
        "#,
        )
        .unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_field_not_found() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            struct Model {
                id: i32,
            }
        "#,
        )
        .unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "nonexistent"));
    }

    #[test]
    fn test_is_field_optional_in_struct_tuple_struct() {
        let struct_item: syn::ItemStruct =
            syn::parse_str("struct TupleStruct(i32, Option<String>);").unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "0"));
    }

    // =========================================================================
    // Tests for convert_seaorm_type_to_chrono edge cases
    // =========================================================================

    #[test]
    fn test_convert_seaorm_type_to_chrono_empty_path() {
        let ty = syn::Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        // Should return the original type unchanged
        assert!(tokens.to_string().is_empty() || tokens.to_string().trim().is_empty());
    }

    // =========================================================================
    // Tests for convert_relation_type_to_schema_with_info
    // =========================================================================

    fn make_test_struct(def: &str) -> syn::ItemStruct {
        syn::parse_str(def).unwrap()
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_empty_segments() {
        let ty = syn::Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_no_angle_brackets() {
        let ty: syn::Type = syn::parse_str("HasOne").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_type_generic() {
        // Test with lifetime generic instead of type
        let ty: syn::Type = syn::parse_str("HasOne<'a>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_inner() {
        // Inner type is a reference, not a path
        let ty: syn::Type = syn::parse_str("HasOne<&str>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_required() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Box"));
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_no_fk() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        // No attributes, so defaults to optional
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert!(info.is_optional); // Default when FK not determinable
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("memos", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert_eq!(info.relation_type, "HasMany");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Vec"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, info) = result.unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(!info.is_optional);
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_unknown_relation() {
        let ty: syn::Type = syn::parse_str("SomeOtherType<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let result =
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], field_name);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_super_path() {
        let ty: syn::Type = syn::parse_str("HasMany<super::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("memos", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, _info) = result.unwrap();
        let output = tokens.to_string();
        // super:: should resolve: crate::models::user -> crate::models::memo
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("memo"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_crate_path() {
        let ty: syn::Type = syn::parse_str("HasMany<crate::models::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let field_name = syn::Ident::new("memos", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, _info) = result.unwrap();
        let output = tokens.to_string();
        // crate:: path should preserve and replace Entity with Schema
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
        let field_name = syn::Ident::new("user", proc_macro2::Span::call_site());
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let result = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            field_name,
        );
        assert!(result.is_some());
        let (tokens, _info) = result.unwrap();
        let output = tokens.to_string();
        // Relative path should be resolved relative to parent
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("user"));
        assert!(output.contains("Schema"));
    }

    // =========================================================================
    // Tests for extract_via_rel (coverage for lines 172-186)
    // =========================================================================

    #[test]
    fn test_extract_via_rel_with_value() {
        // Tests line 178-179: via_rel = "..." found
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(has_many, via_rel = "TargetUser")]
        )];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_via_rel_with_relation_enum() {
        // Tests line 178-179: via_rel alongside other attributes
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(has_many, relation_enum = "TargetUserNotifications", via_rel = "TargetUser")]
        )];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_via_rel_without_via_rel() {
        // Tests: No via_rel attribute present
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(has_many, relation_enum = "Memos")]
        )];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_via_rel_non_sea_orm_attr() {
        // Tests line 172-173: Non-sea_orm attribute returns None
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_via_rel_empty_attrs() {
        // Tests: Empty attributes
        let result = extract_via_rel(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_via_rel_with_other_key_value_pairs() {
        // Tests line 180-182: Other key=value pairs are consumed without error
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id", via_rel = "Author")]
        )];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, Some("Author".to_string()));
    }

    #[test]
    fn test_extract_via_rel_multiple_sea_orm_attrs() {
        // Tests: Multiple sea_orm attributes, via_rel in second one
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(has_many)]),
            syn::parse_quote!(#[sea_orm(via_rel = "Comments")]),
        ];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, Some("Comments".to_string()));
    }
}
