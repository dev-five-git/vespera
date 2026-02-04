//! Type utility functions for schema macro
//!
//! Provides helper functions for type analysis and manipulation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

/// Extract type name from a Type
pub fn extract_type_name(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment (handles paths like crate::User)
            let segment = type_path.path.segments.last().ok_or_else(|| {
                syn::Error::new_spanned(ty, "expected a type path with at least one segment")
            })?;
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "expected a type path (e.g., `User` or `crate::User`)",
        )),
    }
}

/// Check if a type is a qualified path (has multiple segments like crate::models::User)
pub fn is_qualified_path(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path.path.segments.len() > 1,
        _ => false,
    }
}

/// Check if a type is Option<T>
pub fn is_option_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .first()
            .map(|s| s.ident == "Option")
            .unwrap_or(false),
        _ => false,
    }
}

/// Check if a type is a SeaORM relation type (HasOne, HasMany, BelongsTo)
pub fn is_seaorm_relation_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                let ident = segment.ident.to_string();
                matches!(ident.as_str(), "HasOne" | "HasMany" | "BelongsTo")
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if a struct is a SeaORM Model (has #[sea_orm::model] or #[sea_orm(table_name = ...)] attribute)
pub fn is_seaorm_model(struct_item: &syn::ItemStruct) -> bool {
    for attr in &struct_item.attrs {
        // Check for #[sea_orm::model] or #[sea_orm(...)]
        let path = attr.path();
        if path.is_ident("sea_orm") {
            return true;
        }
        // Check for path like sea_orm::model
        let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segments.first().is_some_and(|s| s == "sea_orm") {
            return true;
        }
    }
    false
}

/// Check if a type name is a primitive or well-known type that doesn't need path resolution.
pub fn is_primitive_or_known_type(name: &str) -> bool {
    matches!(
        name,
        // Rust primitives
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            // Common std types
            | "String"
            | "Vec"
            | "Option"
            | "Result"
            | "Box"
            | "Rc"
            | "Arc"
            | "HashMap"
            | "HashSet"
            | "BTreeMap"
            | "BTreeSet"
            // Chrono types
            | "DateTime"
            | "NaiveDateTime"
            | "NaiveDate"
            | "NaiveTime"
            | "Utc"
            | "Local"
            | "FixedOffset"
            // SeaORM types (will be converted separately)
            | "DateTimeWithTimeZone"
            | "DateTimeUtc"
            | "DateTimeLocal"
            // UUID
            | "Uuid"
            // Serde JSON
            | "Value"
    )
}

/// Resolve a simple type to an absolute path using the source module path.
///
/// For example, if source_module_path is ["crate", "models", "memo"] and
/// the type is `MemoStatus`, it returns `crate::models::memo::MemoStatus`.
///
/// If the type is already qualified (has `::`) or is a primitive/known type,
/// returns the original type unchanged.
pub fn resolve_type_to_absolute_path(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return quote! { #ty },
    };

    // If path has multiple segments (already qualified like `crate::foo::Bar`), return as-is
    if type_path.path.segments.len() > 1 {
        return quote! { #ty };
    }

    // Get the single segment
    let segment = match type_path.path.segments.first() {
        Some(s) => s,
        None => return quote! { #ty },
    };

    let ident_str = segment.ident.to_string();

    // If it's a primitive or known type, return as-is
    if is_primitive_or_known_type(&ident_str) {
        return quote! { #ty };
    }

    // If no source module path, return as-is
    if source_module_path.is_empty() {
        return quote! { #ty };
    }

    // Build absolute path: source_module_path + type_name
    let path_idents: Vec<syn::Ident> = source_module_path
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    let type_ident = &segment.ident;
    let args = &segment.arguments;

    quote! { #(#path_idents)::* :: #type_ident #args }
}

/// Extract the module path from a type (excluding the type name itself).
/// e.g., `crate::models::memo::Model` -> ["crate", "models", "memo"]
pub fn extract_module_path(ty: &Type) -> Vec<String> {
    match ty {
        Type::Path(type_path) => {
            let segments: Vec<String> = type_path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            // Return all but the last segment (which is the type name)
            if segments.len() > 1 {
                segments[..segments.len() - 1].to_vec()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Capitalize the first letter of a string.
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    fn empty_type_path() -> syn::Type {
        syn::Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        })
    }

    #[rstest]
    #[case("hello", "Hello")]
    #[case("world", "World")]
    #[case("", "")]
    #[case("a", "A")]
    #[case("ABC", "ABC")]
    #[case("camelCase", "CamelCase")]
    fn test_capitalize_first(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(capitalize_first(input), expected);
    }

    #[rstest]
    #[case("bool", true)]
    #[case("i32", true)]
    #[case("String", true)]
    #[case("Vec", true)]
    #[case("Option", true)]
    #[case("HashMap", true)]
    #[case("DateTime", true)]
    #[case("Uuid", true)]
    #[case("DateTimeWithTimeZone", true)]
    #[case("CustomType", false)]
    #[case("MyStruct", false)]
    fn test_is_primitive_or_known_type(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(is_primitive_or_known_type(name), expected);
    }

    #[test]
    fn test_extract_type_name_simple() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_with_path() {
        let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_non_path_error() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_type_name(&ty);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_qualified_path_simple() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        assert!(!is_qualified_path(&ty));
    }

    #[test]
    fn test_is_qualified_path_crate_path() {
        let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
        assert!(is_qualified_path(&ty));
    }

    #[test]
    fn test_is_qualified_path_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        assert!(!is_qualified_path(&ty));
    }

    #[test]
    fn test_is_option_type_true() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_false() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_vec_false() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_non_path() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_empty_path() {
        let ty = empty_type_path();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_has_one() {
        let ty: syn::Type = syn::parse_str("HasOne<User>").unwrap();
        assert!(is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<Post>").unwrap();
        assert!(is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_belongs_to() {
        let ty: syn::Type = syn::parse_str("BelongsTo<User>").unwrap();
        assert!(is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_regular_type() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_non_path() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        assert!(!is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_relation_type_empty_path() {
        let ty = empty_type_path();
        assert!(!is_seaorm_relation_type(&ty));
    }

    #[test]
    fn test_is_seaorm_model_with_sea_orm_attr() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[sea_orm(table_name = "users")]
            struct Model {
                id: i32,
            }
        "#,
        )
        .unwrap();
        assert!(is_seaorm_model(&struct_item));
    }

    #[test]
    fn test_is_seaorm_model_with_qualified_attr() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[sea_orm::model]
            struct Model {
                id: i32,
            }
        "#,
        )
        .unwrap();
        assert!(is_seaorm_model(&struct_item));
    }

    #[test]
    fn test_is_seaorm_model_regular_struct() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
            #[derive(Debug)]
            struct User {
                id: i32,
            }
        "#,
        )
        .unwrap();
        assert!(!is_seaorm_model(&struct_item));
    }

    #[test]
    fn test_extract_module_path_simple() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        let result = extract_module_path(&ty);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_module_path_qualified() {
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = extract_module_path(&ty);
        assert_eq!(result, vec!["crate", "models", "user"]);
    }

    #[test]
    fn test_extract_module_path_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_module_path(&ty);
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_type_to_absolute_path_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let module_path = vec!["crate".to_string(), "models".to_string()];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert!(output.contains("& str"));
    }

    #[test]
    fn test_resolve_type_to_absolute_path_already_qualified() {
        let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
        let module_path = vec!["crate".to_string(), "other".to_string()];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert!(output.contains("crate :: models :: User"));
    }

    #[test]
    fn test_resolve_type_to_absolute_path_primitive() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        let module_path = vec!["crate".to_string(), "models".to_string()];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert_eq!(output.trim(), "String");
    }

    #[test]
    fn test_resolve_type_to_absolute_path_custom_type() {
        let ty: syn::Type = syn::parse_str("MemoStatus").unwrap();
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert!(output.contains("crate :: models :: memo :: MemoStatus"));
    }

    #[test]
    fn test_resolve_type_to_absolute_path_empty_module() {
        let ty: syn::Type = syn::parse_str("CustomType").unwrap();
        let module_path: Vec<String> = vec![];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert_eq!(output.trim(), "CustomType");
    }

    #[test]
    fn test_resolve_type_to_absolute_path_with_generics() {
        let ty: syn::Type = syn::parse_str("CustomType<T>").unwrap();
        let module_path = vec!["crate".to_string(), "models".to_string()];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert!(output.contains("crate :: models :: CustomType < T >"));
    }

    #[test]
    fn test_resolve_type_to_absolute_path_empty_segments() {
        let ty = empty_type_path();
        let module_path = vec!["crate".to_string()];
        let tokens = resolve_type_to_absolute_path(&ty, &module_path);
        let output = tokens.to_string();
        assert!(output.trim().is_empty());
    }
}
