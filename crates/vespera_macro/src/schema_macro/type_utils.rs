//! Type utility functions for schema macro
//!
//! Provides helper functions for type analysis and manipulation.

use proc_macro2::TokenStream;
use quote::quote;
use serde_json;
use syn::{GenericArgument, PathArguments, Type};

/// SeaORM relation wrapper kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeaOrmRelationKind {
    /// `HasOne<T>` relation.
    HasOne,
    /// `HasMany<T>` relation.
    HasMany,
    /// `BelongsTo<T>` relation.
    BelongsTo,
}

impl SeaOrmRelationKind {
    /// Whether the relation is FK-backed on the current model.
    #[inline]
    pub const fn is_fk_backed(self) -> bool {
        matches!(self, Self::HasOne | Self::BelongsTo)
    }
}

/// Return the final path segment for path-like types.
#[inline]
pub fn last_path_segment(ty: &Type) -> Option<&syn::PathSegment> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    type_path.path.segments.last()
}

/// Return the first generic type argument on a path segment.
#[inline]
pub fn first_generic_type_arg(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Inspect a `syn` type and return the SeaORM relation kind if its final path
/// segment is one of the supported relation wrappers.
pub fn seaorm_relation_kind(ty: &Type) -> Option<SeaOrmRelationKind> {
    let segment = last_path_segment(ty)?;
    if segment.ident == "HasOne" {
        Some(SeaOrmRelationKind::HasOne)
    } else if segment.ident == "HasMany" {
        Some(SeaOrmRelationKind::HasMany)
    } else if segment.ident == "BelongsTo" {
        Some(SeaOrmRelationKind::BelongsTo)
    } else {
        None
    }
}

/// Extract the inner target type of a SeaORM relation wrapper.
pub fn seaorm_relation_inner_type(ty: &Type) -> Option<&Type> {
    let segment = last_path_segment(ty)?;
    seaorm_relation_kind(ty)?;
    first_generic_type_arg(segment)
}

/// Primitive type names shared across parser and schema-macro type parsing.
pub const PRIMITIVE_TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "String", "Decimal",
];

/// Normalize a `TokenStream` or `Type` to a compact string by removing whitespace.
#[inline]
pub fn normalize_token_str(displayable: &impl std::fmt::Display) -> String {
    let s = displayable.to_string();
    // Allocation profile: the `to_string` is unavoidable (`Display` -> owned
    // `String`); a second allocation happens only when whitespace is actually
    // present and must be stripped.  The fast-path gate scans raw bytes rather
    // than chars — every ASCII whitespace byte is a standalone code unit in
    // valid UTF-8, so the byte scan is equivalent to a char scan but skips the
    // per-char UTF-8 decode on the common (whitespace-free) path.
    if s.bytes().any(|b| b.is_ascii_whitespace()) {
        s.replace(|c: char| c.is_ascii_whitespace(), "")
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

/// Extract the inner `T` from `Option<T>`.
///
/// Uses the last path segment so qualified forms such as
/// `std::option::Option<T>` and `core::option::Option<T>` are treated the same
/// as a bare `Option<T>`.
pub fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Check if a type is `Option<T>`.
pub fn is_option_type(ty: &Type) -> bool {
    option_inner(ty).is_some()
}

/// Check if a type is a `SeaORM` relation type (`HasOne`, `HasMany`, `BelongsTo`)
pub fn is_seaorm_relation_type(ty: &Type) -> bool {
    seaorm_relation_kind(ty).is_some()
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
            // Decimal (rust_decimal / sea_orm re-export)
            | "Decimal"
            // Serde JSON
            | "Value"
    )
}

fn resolve_public_type_path(name: &str) -> Option<TokenStream> {
    match name {
        // SeaORM re-exports `serde_json::Value` as `Json`; emit a stable public path.
        "Json" | "Value" => Some(quote! { vespera::serde_json::Value }),
        _ => None,
    }
}

fn normalize_known_type_in_generic(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let Type::Path(type_path) = ty else {
        return quote! { #ty };
    };

    let Some(segment) = type_path.path.segments.last() else {
        return quote! { #ty };
    };

    let ident_str = segment.ident.to_string();

    if let Some(public_path) = resolve_public_type_path(&ident_str) {
        return quote! { #public_path };
    }

    if type_path.path.segments.len() > 1 {
        let rendered_segments: Vec<_> = type_path
            .path
            .segments
            .iter()
            .map(|segment| {
                let ident = &segment.ident;
                let args = render_path_arguments(&segment.arguments, source_module_path);
                quote! { #ident #args }
            })
            .collect();

        if type_path.path.leading_colon.is_some() {
            return quote! { :: #(#rendered_segments)::* };
        }

        return quote! { #(#rendered_segments)::* };
    }

    if is_primitive_or_known_type(&ident_str) {
        let ident = &segment.ident;
        let args = render_path_arguments(&segment.arguments, source_module_path);
        return quote! { #ident #args };
    }

    quote! { #ty }
}

fn render_path_arguments(args: &PathArguments, source_module_path: &[String]) -> TokenStream {
    match args {
        PathArguments::None => quote! {},
        PathArguments::AngleBracketed(angle_args) => {
            let rendered_args: Vec<_> = angle_args
                .args
                .iter()
                .map(|arg| {
                    if let GenericArgument::Type(inner_ty) = arg {
                        let resolved =
                            normalize_known_type_in_generic(inner_ty, source_module_path);
                        quote! { #resolved }
                    } else {
                        quote! { #arg }
                    }
                })
                .collect();

            quote! { <#(#rendered_args),*> }
        }
        PathArguments::Parenthesized(_) => quote! { #args },
    }
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

    if type_path.path.segments.is_empty() {
        return quote! { #ty };
    }

    // If path has multiple segments (already qualified like `crate::foo::Bar`), return as-is
    if type_path.path.segments.len() > 1 {
        let rendered_segments: Vec<_> = type_path
            .path
            .segments
            .iter()
            .map(|segment| {
                let ident = &segment.ident;
                let args = render_path_arguments(&segment.arguments, source_module_path);
                quote! { #ident #args }
            })
            .collect();

        if type_path.path.leading_colon.is_some() {
            return quote! { :: #(#rendered_segments)::* };
        }

        return quote! { #(#rendered_segments)::* };
    }

    // Safe after the empty-path early return above.
    let segment = type_path
        .path
        .segments
        .first()
        .expect("type path should have at least one segment");

    let ident_str = segment.ident.to_string();
    let args = render_path_arguments(&segment.arguments, source_module_path);

    if let Some(public_path) = resolve_public_type_path(&ident_str) {
        return quote! { #public_path };
    }

    // If it's a primitive or known type, return as-is
    if is_primitive_or_known_type(&ident_str) {
        let type_ident = &segment.ident;
        return quote! { #type_ident #args };
    }

    // If no source module path, return as-is
    if source_module_path.is_empty() {
        let type_ident = &segment.ident;
        return quote! { #type_ident #args };
    }

    // Build absolute path: source_module_path + type_name
    let path_idents: Vec<syn::Ident> = source_module_path
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    let type_ident = &segment.ident;

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
    // `segments.last()` yields `None` for an empty path, so the let-chain
    // both replaces the prior `is_empty()` guard + `unwrap()` and skips the
    // per-call `ident.to_string()` allocation (`Ident: PartialEq<str>`).
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return segment.ident == "HashMap" || segment.ident == "BTreeMap";
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
                "Option" => Some(serde_json::Value::Null),
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
mod tests;
