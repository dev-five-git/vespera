//! Type utility functions for schema macro
//!
//! Provides helper functions for type analysis and manipulation.

use proc_macro2::TokenStream;
use quote::quote;
use serde_json;
use syn::Type;

/// Normalize a `TokenStream` or `Type` to a compact string by removing spaces.
///
/// This replaces the common `.to_string().replace(' ', "")` pattern used throughout
/// the codebase to produce deterministic path strings for comparison and cache keys.
#[inline]
pub fn normalize_token_str(displayable: &impl std::fmt::Display) -> String {
    let s = displayable.to_string();
    if s.contains(' ') {
        s.replace(' ', "")
    } else {
        s
    }
}

/// Extract type name from a Type
pub fn extract_type_name(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment (handles paths like crate::User)
            let segment = type_path.path.segments.last().ok_or_else(|| syn::Error::new_spanned(ty, "extract_type_name: type path has no segments. Provide a valid type like `User` or `crate::models::User`."))?;
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "extract_type_name: expected a type path, not a reference or other type. Use a type like `User` or `crate::User` instead of `&User`.",
        )),
    }
}

/// Check if a type is a qualified path (has multiple segments like `crate::models::User`)
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
            .is_some_and(|s| s.ident == "Option"),
        _ => false,
    }
}

/// Check if a type is a `SeaORM` relation type (`HasOne`, `HasMany`, `BelongsTo`)
pub fn is_seaorm_relation_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path.path.segments.last().is_some_and(|segment| {
            let ident = segment.ident.to_string();
            matches!(ident.as_str(), "HasOne" | "HasMany" | "BelongsTo")
        }),
        _ => false,
    }
}

/// Check if a struct is a `SeaORM` Model (has #[`sea_orm::model`] or #[`sea_orm(table_name` = ...)] attribute)
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
            | "Date"  // SeaORM re-export of chrono::NaiveDate
            | "Time"  // SeaORM re-export of chrono::NaiveTime
            // UUID
            | "Uuid"
            // Serde JSON
            | "Value"
    )
}

/// Resolve a simple type to an absolute path using the source module path.
///
/// For example, if `source_module_path` is `["crate", "models", "memo"]` and
/// the type is `MemoStatus`, it returns `crate::models::memo::MemoStatus`.
///
/// If the type is already qualified (has `::`) or is a primitive/known type,
/// returns the original type unchanged.
pub fn resolve_type_to_absolute_path(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let Type::Path(type_path) = ty else {
        return quote! { #ty };
    };

    // If path has multiple segments (already qualified like `crate::foo::Bar`), return as-is
    if type_path.path.segments.len() > 1 {
        return quote! { #ty };
    }

    // Get the single segment
    let Some(segment) = type_path.path.segments.first() else {
        return quote! { #ty };
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
/// e.g., `crate::models::memo::Model` -> `["crate", "models", "memo"]`
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
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Convert `snake_case` to `PascalCase`.
/// e.g., "`target_user_id`" -> "`TargetUserId`", "comments" -> "Comments"
pub fn snake_to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

/// Check if a type is `HashMap` or `BTreeMap`
pub fn is_map_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            let segment = path.segments.last().unwrap();
            let ident_str = segment.ident.to_string();
            return ident_str == "HashMap" || ident_str == "BTreeMap";
        }
    }
    false
}

/// Check if a type is a primitive type OR a known well-behaved container.
///
/// This checks the outer type name against a list of known types (primitives, std containers, etc.).
/// Types like `Vec`, `Option`, `HashMap` are considered primitive-like regardless of their contents.
pub fn is_primitive_like(ty: &Type) -> bool {
    is_primitive_or_known_type(&extract_type_name(ty).unwrap_or_default())
}

/// Get type-specific default value for simple #[serde(default)]
pub fn get_type_default(ty: &Type) -> Option<serde_json::Value> {
    match ty {
        Type::Path(type_path) => type_path.path.segments.last().and_then(|segment| {
            match segment.ident.to_string().as_str() {
                "String" => Some(serde_json::Value::String(String::new())),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "Decimal" => {
                    Some(serde_json::Value::Number(serde_json::Number::from(0)))
                }
                "f32" | "f64" => Some(serde_json::Value::Number(
                    serde_json::Number::from_f64(0.0)
                        .unwrap_or_else(|| serde_json::Number::from(0)),
                )),
                "bool" => Some(serde_json::Value::Bool(false)),
                "Uuid" => Some(serde_json::Value::String(
                    "00000000-0000-0000-0000-000000000000".to_string(),
                )),
                "DateTime" | "DateTimeWithTimeZone" | "DateTimeUtc" => Some(
                    serde_json::Value::String("1970-01-01T00:00:00+00:00".to_string()),
                ),
                "NaiveDateTime" => {
                    Some(serde_json::Value::String("1970-01-01T00:00:00".to_string()))
                }
                "NaiveDate" => Some(serde_json::Value::String("1970-01-01".to_string())),
                "NaiveTime" | "Time" => Some(serde_json::Value::String("00:00:00".to_string())),
                _ => None,
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
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
    #[case("comments", "Comments")]
    #[case("target_user_notifications", "TargetUserNotifications")]
    #[case("memo_comments", "MemoComments")]
    #[case("", "")]
    #[case("a", "A")]
    #[case("user_id", "UserId")]
    #[case("ABC", "ABC")]
    fn test_snake_to_pascal_case(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(snake_to_pascal_case(input), expected);
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
            r"
            #[sea_orm::model]
            struct Model {
                id: i32,
            }
        ",
        )
        .unwrap();
        assert!(is_seaorm_model(&struct_item));
    }

    #[test]
    fn test_is_seaorm_model_regular_struct() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r"
            #[derive(Debug)]
            struct User {
                id: i32,
            }
        ",
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

    #[rstest]
    #[case("HashMap<String, i32>", true)]
    #[case("BTreeMap<String, i32>", true)]
    #[case("String", false)]
    #[case("Vec<String>", false)]
    fn test_is_map_type(#[case] type_str: &str, #[case] expected: bool) {
        let ty: syn::Type = syn::parse_str(type_str).unwrap();
        assert_eq!(is_map_type(&ty), expected);
    }

    #[rstest]
    #[case("String", Some(serde_json::Value::String(String::new())))]
    #[case("i32", Some(serde_json::Value::Number(serde_json::Number::from(0))))]
    #[case("bool", Some(serde_json::Value::Bool(false)))]
    #[case("f64", Some(serde_json::Value::Number(serde_json::Number::from_f64(0.0).unwrap())))]
    #[case("CustomType", None)]
    fn test_get_type_default(#[case] type_str: &str, #[case] expected: Option<serde_json::Value>) {
        let ty: syn::Type = syn::parse_str(type_str).unwrap();
        let result = get_type_default(&ty);
        match expected {
            Some(exp) => {
                assert!(result.is_some());
                let res = result.unwrap();
                assert_eq!(res, exp);
            }
            None => assert!(result.is_none()),
        }
    }

    #[test]
    fn test_is_primitive_like_true() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_vec_of_primitives() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_option_of_primitives() {
        let ty: syn::Type = syn::parse_str("Option<i32>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_custom_type() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        assert!(!is_primitive_like(&ty));
    }

    // Edge case tests for type_utils functions

    #[test]
    fn test_extract_type_name_empty_path_error() {
        let ty = empty_type_path();
        let result = extract_type_name(&ty);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("type path has no segments")
        );
    }

    #[test]
    fn test_is_map_type_empty_path() {
        let ty = empty_type_path();
        assert!(!is_map_type(&ty));
    }

    #[test]
    fn test_is_primitive_like_vec_string() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_vec_i32() {
        let ty: syn::Type = syn::parse_str("Vec<i32>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_option_string() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_option_bool() {
        let ty: syn::Type = syn::parse_str("Option<bool>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_vec_of_custom_type() {
        // Vec is a known type, so Vec<User> is considered primitive-like
        let ty: syn::Type = syn::parse_str("Vec<User>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_option_of_custom_type() {
        // Option is a known type, so Option<User> is considered primitive-like
        let ty: syn::Type = syn::parse_str("Option<User>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_nested_vec_option() {
        let ty: syn::Type = syn::parse_str("Vec<Option<String>>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_nested_option_vec() {
        let ty: syn::Type = syn::parse_str("Option<Vec<i32>>").unwrap();
        assert!(is_primitive_like(&ty));
    }

    #[test]
    fn test_is_primitive_like_vec_of_datetime() {
        let ty: syn::Type = syn::parse_str("Vec<DateTime<Utc>>").unwrap();
        assert!(is_primitive_like(&ty));
    }
}
