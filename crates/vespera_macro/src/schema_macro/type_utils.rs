//! Type utility functions for schema macro
//!
//! Provides helper functions for type analysis and manipulation.

use proc_macro2::TokenStream;
use quote::quote;
use serde_json;
use syn::{GenericArgument, PathArguments, Type};

use super::module_scope::{
    ModuleScope, Resolved, module_scope, path_tokens, resolve_name, segment_ident,
};

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

/// Re-express `ty`, written inside the module `source_module_path`, so it
/// names the same type from ANY module of the crate (the generated struct
/// lives at the macro call site, not next to its source):
///
/// - names the module imports with `use` resolve through the import, never
///   through the module path (a private import re-exported that way is
///   `E0603`), locally defined items through the module path, and names a
///   crate-internal glob import brings in through that glob's module;
/// - relative multi-segment paths (`enums::Status`, `super::x::Y`) are
///   re-rooted; external crate paths stay as written;
/// - generic arguments are resolved recursively (`Option<Vec<T>>`,
///   `HashMap<K, V>`, references, tuples, arrays);
/// - SeaORM / chrono aliases become `vespera::chrono` types and the
///   `serde_json::Value` aliases (`Json`, `JsonValue`, `Value`) become
///   `vespera::serde_json::Value` — unless the module defines or imports a
///   type of that name itself.
pub fn resolve_type_to_absolute_path(ty: &Type, source_module_path: &[String]) -> TokenStream {
    resolve_type_with_params(ty, source_module_path, &[])
}

/// [`resolve_type_to_absolute_path`] that leaves the listed generic
/// parameter names untouched (the source struct's own `T`, ...).
pub fn resolve_type_with_params(
    ty: &Type,
    source_module_path: &[String],
    generic_params: &[String],
) -> TokenStream {
    let scope = module_scope(source_module_path);
    TypeResolver {
        module: source_module_path,
        scope: scope.as_deref(),
        generic_params,
    }
    .ty(ty)
}

struct TypeResolver<'a> {
    module: &'a [String],
    scope: Option<&'a ModuleScope>,
    generic_params: &'a [String],
}

impl TypeResolver<'_> {
    fn ty(&self, ty: &Type) -> TokenStream {
        match ty {
            Type::Path(type_path) if type_path.qself.is_none() => {
                self.path(&type_path.path).unwrap_or_else(|| quote! { #ty })
            }
            Type::Reference(reference) => {
                let lifetime = &reference.lifetime;
                let mutability = &reference.mutability;
                let elem = self.ty(&reference.elem);
                quote! { & #lifetime #mutability #elem }
            }
            Type::Slice(slice) => {
                let elem = self.ty(&slice.elem);
                quote! { [#elem] }
            }
            Type::Array(array) => {
                let elem = self.ty(&array.elem);
                let len = &array.len;
                quote! { [#elem; #len] }
            }
            Type::Tuple(tuple) => {
                let elems: Vec<TokenStream> = tuple.elems.iter().map(|e| self.ty(e)).collect();
                if elems.len() == 1 {
                    quote! { (#(#elems),*,) }
                } else {
                    quote! { (#(#elems),*) }
                }
            }
            Type::Paren(paren) => {
                let elem = self.ty(&paren.elem);
                quote! { (#elem) }
            }
            _ => quote! { #ty },
        }
    }

    fn args(&self, args: &PathArguments) -> TokenStream {
        match args {
            PathArguments::None => TokenStream::new(),
            PathArguments::AngleBracketed(angle) => {
                let items = angle.args.iter().map(|arg| match arg {
                    GenericArgument::Type(inner) => self.ty(inner),
                    other => quote! { #other },
                });
                let colon = &angle.colon2_token;
                quote! { #colon <#(#items),*> }
            }
            // `Fn(A) -> B` bounds: not a serializable field type.
            PathArguments::Parenthesized(_) => quote! { #args },
        }
    }

    fn path(&self, path: &syn::Path) -> Option<TokenStream> {
        let last = path.segments.last()?;
        if path.leading_colon.is_none() && path.segments.len() == 1 {
            return Some(self.single(last));
        }
        let name = strip_raw(&last.ident);
        if SEA_ORM_PATH_ALIASES.contains(&name.as_str())
            && let Some(converted) = self.alias(&name, &last.arguments, true)
        {
            return Some(converted);
        }
        let mut segments: Vec<(String, TokenStream)> = path
            .segments
            .iter()
            .map(|segment| (strip_raw(&segment.ident), self.args(&segment.arguments)))
            .collect();
        if path.leading_colon.is_some() {
            segments[0].0.insert_str(0, "::");
        } else if let Resolved::Path(prefix) =
            resolve_name(std::slice::from_ref(&segments[0].0), self.module)
        {
            let first_args = segments.remove(0).1;
            let mut rooted: Vec<(String, TokenStream)> = prefix
                .into_iter()
                .map(|s| (s, TokenStream::new()))
                .collect();
            if let Some(last) = rooted.last_mut() {
                last.1 = first_args;
            }
            rooted.extend(segments);
            segments = rooted;
        }
        Some(render_segments(&segments))
    }

    fn single(&self, segment: &syn::PathSegment) -> TokenStream {
        let name = strip_raw(&segment.ident);
        let ident = &segment.ident;
        let args = self.args(&segment.arguments);
        if self.generic_params.contains(&name) {
            return quote! { #ident #args };
        }
        if let Some(scope) = self.scope {
            if let Some(import) = scope.imports.get(&name) {
                if first_named(import) == Some("sea_orm")
                    && let Some(converted) = self.alias(&name, &segment.arguments, true)
                {
                    return converted;
                }
                let path = path_tokens(import);
                return quote! { #path #args };
            }
            if scope.locals.contains(&name) {
                return self.module_qualified(ident, &args);
            }
        }
        let sea_orm_scope = self.scope.is_some_and(ModuleScope::has_sea_orm_glob);
        if let Some(converted) = self.alias(&name, &segment.arguments, sea_orm_scope) {
            return converted;
        }
        if matches!(name.as_str(), "Uuid" | "Decimal")
            && let Some(glob) = self.scope.and_then(|scope| {
                scope
                    .globs
                    .iter()
                    .find(|glob| first_named(glob) == Some("sea_orm"))
            })
        {
            let base = path_tokens(glob);
            return quote! { #base :: #ident };
        }
        if is_primitive_or_known_type(&name) {
            return quote! { #ident #args };
        }
        if let Resolved::Path(path) = resolve_name(std::slice::from_ref(&name), self.module) {
            let path = path_tokens(&path);
            return quote! { #path #args };
        }
        if self.module.is_empty() {
            return quote! { #ident #args };
        }
        self.module_qualified(ident, &args)
    }

    fn module_qualified(&self, ident: &syn::Ident, args: &TokenStream) -> TokenStream {
        let module = path_tokens(self.module);
        quote! { #module :: #ident #args }
    }

    /// SeaORM / chrono / `serde_json` / multipart aliases. `Date` / `Time`
    /// are SeaORM's only in a module glob-importing a SeaORM prelude (they
    /// are `time` crate names too).
    fn alias(&self, name: &str, args: &PathArguments, sea_orm_scope: bool) -> Option<TokenStream> {
        let bare = args.is_none();
        Some(match name {
            "DateTimeWithTimeZone" if bare => {
                quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
            }
            "DateTimeUtc" if bare => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
            "DateTimeLocal" if bare => {
                quote! { vespera::chrono::DateTime<vespera::chrono::Local> }
            }
            // chrono's `DateTime` always takes a time zone: the bare name is
            // SeaORM's alias of `NaiveDateTime`.
            "DateTime" if bare => quote! { vespera::chrono::NaiveDateTime },
            "DateTime" => {
                let args = self.args(args);
                quote! { vespera::chrono::DateTime #args }
            }
            "Date" if bare && sea_orm_scope => quote! { vespera::chrono::NaiveDate },
            "Time" if bare && sea_orm_scope => quote! { vespera::chrono::NaiveTime },
            "NaiveDateTime" | "NaiveDate" | "NaiveTime" | "Utc" | "Local" | "FixedOffset"
                if bare =>
            {
                let ident = syn::Ident::new(name, proc_macro2::Span::call_site());
                quote! { vespera::chrono::#ident }
            }
            "Json" | "JsonValue" | "Value" if bare => quote! { vespera::serde_json::Value },
            "FieldData" => {
                let args = self.args(args);
                quote! { vespera::multipart::FieldData #args }
            }
            "NamedTempFile" => quote! { vespera::tempfile::NamedTempFile },
            _ => return None,
        })
    }
}

/// Aliases converted whatever path spells them (`sea_orm::prelude::DateTimeUtc`).
const SEA_ORM_PATH_ALIASES: &[&str] = &[
    "DateTimeWithTimeZone",
    "DateTimeUtc",
    "DateTimeLocal",
    "FieldData",
    "NamedTempFile",
];

fn first_named(path: &[String]) -> Option<&str> {
    path.iter().find(|s| !s.is_empty()).map(String::as_str)
}

fn strip_raw(ident: &syn::Ident) -> String {
    crate::parser::strip_raw_prefix_owned(ident.to_string())
}

fn render_segments(segments: &[(String, TokenStream)]) -> TokenStream {
    let mut out = TokenStream::new();
    for (index, (name, args)) in segments.iter().enumerate() {
        let name = if let Some(rest) = name.strip_prefix("::") {
            out.extend(quote! { :: });
            rest
        } else {
            if index > 0 {
                out.extend(quote! { :: });
            }
            name.as_str()
        };
        let ident = segment_ident(name);
        out.extend(quote! { #ident #args });
    }
    out
}

// Extract the module path from a type (excluding the type name itself).
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
