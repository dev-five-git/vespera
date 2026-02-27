//! `SeaORM` and Chrono type conversions
//!
//! Handles conversion of `SeaORM` relation types and datetime types to their
//! schema equivalents.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::type_utils::{is_option_type, resolve_type_to_absolute_path};

/// Relation field info for generating `from_model` code
#[derive(Clone)]
pub struct RelationFieldInfo {
    /// Field name in the generated struct
    pub field_name: syn::Ident,
    /// Relation type: "`HasOne`", "`HasMany`", or "`BelongsTo`"
    pub relation_type: String,
    /// Target Schema path (e.g., `crate::models::user::Schema`)
    pub schema_path: TokenStream,
    /// Whether the relation is optional
    pub is_optional: bool,
    /// If Some, this relation has circular refs and uses an inline type
    /// Contains: (`inline_type_name`, `circular_fields_to_exclude`)
    pub inline_type_info: Option<(syn::Ident, Vec<String>)>,
    /// The `relation_enum` attribute value (e.g., "`TargetUser`", "`CreatedByUser`")
    /// When present, indicates multiple relations to the same Entity type exist
    pub relation_enum: Option<String>,
    /// The FK column name from `from` attribute (e.g., "`user_id`", "`target_user_id`")
    pub fk_column: Option<String>,
    /// The `via_rel` attribute value for `HasMany` relations (e.g., "`TargetUser`")
    /// This specifies which Relation variant on the TARGET entity to use
    pub via_rel: Option<String>,
}

/// Convert `SeaORM` datetime types to chrono equivalents.
///
/// This allows generated schemas to use standard chrono types instead of
/// requiring `use sea_orm::entity::prelude::DateTimeWithTimeZone`.
///
/// Conversions:
/// - `DateTimeWithTimeZone` -> `chrono::DateTime<chrono::FixedOffset>`
/// - `DateTimeUtc` -> `chrono::DateTime<chrono::Utc>`
/// - `DateTimeLocal` -> `chrono::DateTime<chrono::Local>`
/// - `DateTime` (`SeaORM`) -> `chrono::NaiveDateTime`
/// - `Date` (`SeaORM`) -> `chrono::NaiveDate`
/// - `Time` (`SeaORM`) -> `chrono::NaiveTime`
///
/// Returns the original type as `TokenStream` if not a `SeaORM` datetime type.
pub fn convert_seaorm_type_to_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let Type::Path(type_path) = ty else {
        return quote! { #ty };
    };

    let Some(segment) = type_path.path.segments.last() else {
        return quote! { #ty };
    };

    let ident_str = segment.ident.to_string();

    match ident_str.as_str() {
        // Use vespera::chrono to avoid requiring users to add chrono dependency
        "DateTimeWithTimeZone" => {
            quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
        }
        "DateTimeUtc" => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
        "DateTimeLocal" => quote! { vespera::chrono::DateTime<vespera::chrono::Local> },
        // axum_typed_multipart types - resolve via vespera re-exports
        "FieldData" => {
            // Preserve inner generic: FieldData<T> → vespera::axum_typed_multipart::FieldData<T>
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                let inner_args: Vec<_> = args
                    .args
                    .iter()
                    .map(|arg| {
                        if let syn::GenericArgument::Type(inner_ty) = arg {
                            let converted =
                                convert_seaorm_type_to_chrono(inner_ty, source_module_path);
                            quote! { #converted }
                        } else {
                            quote! { #arg }
                        }
                    })
                    .collect();
                quote! { vespera::axum_typed_multipart::FieldData<#(#inner_args),*> }
            } else {
                quote! { vespera::axum_typed_multipart::FieldData }
            }
        }
        "NamedTempFile" => quote! { vespera::tempfile::NamedTempFile },
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
/// (like `crate::models::memo::MemoStatus`) using `source_module_path`.
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

/// Extract the "from" field name from a `sea_orm` `belongs_to` attribute.
/// e.g., `#[sea_orm(belongs_to, from = "user_id", to = "id")]` -> `Some("user_id`")
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
                drop(
                    meta.value()
                        .and_then(syn::parse::ParseBuffer::parse::<syn::LitStr>),
                );
            }
            Ok(())
        });
        from_field
    })
}

/// Extract the "`relation_enum`" value from a `sea_orm` attribute.
/// e.g., `#[sea_orm(belongs_to, relation_enum = "TargetUser", from = "target_user_id")]` -> Some("TargetUser")
///
/// When `relation_enum` is present, it indicates that multiple relations to the same
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
                drop(
                    meta.value()
                        .and_then(syn::parse::ParseBuffer::parse::<syn::LitStr>),
                );
            }
            Ok(())
        });
        relation_enum_value
    })
}

/// Extract the "`via_rel`" value from a `sea_orm` attribute.
/// e.g., `#[sea_orm(has_many, relation_enum = "TargetUser", via_rel = "TargetUser")]` -> Some("TargetUser")
///
/// For `HasMany` relations with `relation_enum`, `via_rel` specifies which Relation variant
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
                drop(
                    meta.value()
                        .and_then(syn::parse::ParseBuffer::parse::<syn::LitStr>),
                );
            }
            Ok(())
        });
        via_rel_value
    })
}

/// Extract `default_value` from a `sea_orm` attribute.
/// e.g., `#[sea_orm(default_value = 0.7)]` -> `Some("0.7")`
/// e.g., `#[sea_orm(default_value = "active")]` -> `Some("active")`
pub fn extract_sea_orm_default_value(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }

        // Use raw token string parsing to handle all literal types
        // (parse_nested_meta can't easily parse non-string literals after `=`)
        let syn::Meta::List(meta_list) = &attr.meta else {
            continue;
        };
        let tokens = meta_list.tokens.to_string();

        if let Some(start) = tokens.find("default_value") {
            let remaining = &tokens[start + "default_value".len()..];
            let remaining = remaining.trim_start();
            if let Some(after_eq) = remaining.strip_prefix('=') {
                let value_str = after_eq.trim_start();
                // Extract value until comma or end of tokens
                let end = value_str.find(',').unwrap_or(value_str.len());
                let raw_value = value_str[..end].trim();

                if raw_value.is_empty() {
                    continue;
                }

                // If quoted string, strip quotes and return inner value
                if let Some(inner) = raw_value
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                {
                    return Some(inner.to_string());
                }
                // Numeric, bool, or other literal — return as-is
                return Some(raw_value.to_string());
            }
        }
    }
    None
}

/// Check if a `sea_orm(default_value)` is a SQL function (e.g., `"NOW()"`, `"CURRENT_TIMESTAMP()"`, `"UUID()"`)
/// that cannot be converted to a Rust default value.
///
/// Detection: any value containing parentheses is treated as a SQL function call.
pub fn is_sql_function_default(value: &str) -> bool {
    value.contains('(')
}

/// Check if a field has `#[sea_orm(primary_key)]`.
///
/// Primary keys in SeaORM imply auto-increment by default,
/// meaning the database provides a value even when the client omits it.
pub fn has_sea_orm_primary_key(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }
        let syn::Meta::List(meta_list) = &attr.meta else {
            continue;
        };
        let tokens = meta_list.tokens.to_string();
        if tokens.contains("primary_key") {
            return true;
        }
    }
    false
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

/// Convert a `SeaORM` relation type to a Schema type AND return relation info.
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
/// Returns (`TokenStream`, `RelationFieldInfo`) on success for use in `from_model` generation.
#[allow(clippy::too_many_lines)]
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

    // Check if this is a relation type with generic argument
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };

    // Get the inner Entity type
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };

    // Extract the path and convert to absolute Schema path
    let Type::Path(inner_path) = inner_ty else {
        return None;
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
        let mut abs = Vec::with_capacity(parent_path_len + segments.len() - super_count);
        abs.extend_from_slice(&source_module_path[..parent_path_len]);
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
        let mut abs = Vec::with_capacity(parent_path_len + segments.len());
        abs.extend_from_slice(&source_module_path[..parent_path_len]);
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
                .is_none_or(|f| is_field_optional_in_struct(parsed_struct, f)); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasOne".to_string(),
                schema_path,
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
                schema_path,
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
                .is_none_or(|f| is_field_optional_in_struct(parsed_struct, f)); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "BelongsTo".to_string(),
                schema_path,
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
            r"
            struct Model {
                id: i32,
                user_id: Option<i32>,
            }
        ",
        )
        .unwrap();
        assert!(is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_required() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            struct Model {
                id: i32,
                user_id: i32,
            }
        ",
        )
        .unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_field_not_found() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            struct Model {
                id: i32,
            }
        ",
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
    // Tests for FieldData/NamedTempFile type conversion
    // =========================================================================

    #[test]
    fn test_convert_seaorm_type_field_data_with_generic() {
        // FieldData<NamedTempFile> → vespera::axum_typed_multipart::FieldData<vespera::tempfile::NamedTempFile>
        let ty: syn::Type = syn::parse_str("FieldData<NamedTempFile>").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(
            output.contains("vespera :: axum_typed_multipart :: FieldData"),
            "Should resolve FieldData via vespera re-export: {output}"
        );
        assert!(
            output.contains("vespera :: tempfile :: NamedTempFile"),
            "Should resolve inner NamedTempFile via vespera re-export: {output}"
        );
    }

    #[test]
    fn test_convert_seaorm_type_field_data_without_generic() {
        // FieldData (no generics) → vespera::axum_typed_multipart::FieldData
        let ty: syn::Type = syn::parse_str("FieldData").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(
            output.contains("vespera :: axum_typed_multipart :: FieldData"),
            "Should resolve bare FieldData: {output}"
        );
        // Should NOT contain nested generic
        assert!(
            !output.contains("NamedTempFile"),
            "Bare FieldData should not have NamedTempFile: {output}"
        );
    }

    #[test]
    fn test_convert_seaorm_type_field_data_with_non_type_generic() {
        // FieldData with a non-Type generic arg (e.g., lifetime) should use fallback quote
        let ty: syn::Type = syn::parse_str("FieldData<'a>").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert!(
            output.contains("vespera :: axum_typed_multipart :: FieldData"),
            "Should still resolve FieldData: {output}"
        );
    }

    #[test]
    fn test_convert_seaorm_type_named_temp_file() {
        // NamedTempFile → vespera::tempfile::NamedTempFile
        let ty: syn::Type = syn::parse_str("NamedTempFile").unwrap();
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        let output = tokens.to_string();
        assert_eq!(output.trim(), "vespera :: tempfile :: NamedTempFile");
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
    // Tests for extract_via_rel
    // =========================================================================

    #[test]
    fn test_extract_via_rel_with_value() {
        // Tests: via_rel = "..." found
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(has_many, via_rel = "TargetUser")]
        )];
        let result = extract_via_rel(&attrs);
        assert_eq!(result, Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_via_rel_with_relation_enum() {
        // Tests: via_rel alongside other attributes
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
        // Tests: Non-sea_orm attribute returns None
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
        // Tests: Other key=value pairs are consumed without error
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

    #[test]
    fn test_extract_sea_orm_default_value_float() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(default_value = 0.7)]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, Some("0.7".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_int() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(default_value = 42)]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_string() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(default_value = "active")]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, Some("active".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_bool() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(default_value = true)]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_with_other_attrs() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(column_type = "Decimal(Some((10, 2)))", default_value = 0.7)]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, Some("0.7".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_none() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(column_type = "Text")]
        )];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_non_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_empty_attrs() {
        let result = extract_sea_orm_default_value(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_non_list_meta() {
        // #[sea_orm] as a path attribute (non-Meta::List) — line 222 branch
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm])];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_empty_value_after_equals() {
        // default_value = , (empty value) — line 236 branch
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = )])];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_no_default_value_key() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(primary_key, auto_increment)])];
        let result = extract_sea_orm_default_value(&attrs);
        assert_eq!(result, None);
    }

    // =========================================================================
    // Tests for is_sql_function_default
    // =========================================================================

    #[rstest]
    #[case("NOW()", true)]
    #[case("CURRENT_TIMESTAMP()", true)]
    #[case("UUID()", true)]
    #[case("gen_random_uuid()", true)]
    #[case("0.7", false)]
    #[case("42", false)]
    #[case("true", false)]
    #[case("draft", false)]
    #[case("active", false)]
    fn test_is_sql_function_default(#[case] value: &str, #[case] expected: bool) {
        assert_eq!(is_sql_function_default(value), expected);
    }

    // =========================================================================
    // Tests for has_sea_orm_primary_key
    // =========================================================================

    #[test]
    fn test_has_sea_orm_primary_key_true() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(primary_key)])];
        assert!(has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_with_other_attrs() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(primary_key, default_value = "gen_random_uuid()")])];
        assert!(has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_false() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
        assert!(!has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        assert!(!has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_empty_attrs() {
        assert!(!has_sea_orm_primary_key(&[]));
    }

    #[test]
    fn test_has_sea_orm_primary_key_non_list_meta() {
        // #[sea_orm = "value"] is a NameValue meta, not a List — should be skipped
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm = "something"])];
        assert!(!has_sea_orm_primary_key(&attrs));
    }
}
