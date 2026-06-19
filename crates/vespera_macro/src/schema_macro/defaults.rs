//! SeaORM default-value attribute generation.
//!
//! Translates `#[sea_orm(default_value = ...)]` / `#[sea_orm(primary_key)]`
//! on source fields into `#[serde(default = "...")]` + `#[schema(default = "...")]`
//! attributes (plus companion default functions) on the generated struct.

use proc_macro2::TokenStream;
use quote::quote;

use super::seaorm::{
    extract_sea_orm_default_value, has_sea_orm_primary_key, is_sql_function_default,
};
use super::type_utils;
use crate::parser::extract_default;

/// Generate `#[serde(default = "...")]` and `#[schema(default = "...")]` attributes
/// from `#[sea_orm(default_value = ...)]` or `#[sea_orm(primary_key)]` on source fields.
///
/// Returns `(serde_default_attr, schema_default_attr)` as `TokenStream`s.
/// - `serde_default_attr`: `#[serde(default = "default_structname_field")]` for deserialization
/// - `schema_default_attr`: `#[schema(default = "value")]` for OpenAPI default value
///
/// Also generates a companion default function and appends it to `default_functions`.
///
/// Handles three categories of defaults:
/// 1. **Literal defaults** (`default_value = "42"`, `"draft"`, `0.7`):
///    Generates parse-based default function + schema default.
/// 2. **SQL function defaults** (`default_value = "NOW()"`, `"gen_random_uuid()"`):
///    Generates type-specific default function + schema default with type's zero value.
/// 3. **Primary key** (implicit auto-increment):
///    Treated as having an implicit default — generates type-specific default.
///
/// Skips serde default generation when:
/// - The field is wrapped in `Option` (partial mode or already optional)
/// - The field already has `#[serde(default)]`
/// - For literal defaults: the field type doesn't implement `FromStr`
pub(super) fn generate_sea_orm_default_attrs(
    original_attrs: &[syn::Attribute],
    struct_name: &syn::Ident,
    field_name: &str,
    original_ty: &syn::Type,
    field_ty: &dyn quote::ToTokens,
    is_optional_or_partial: bool,
    default_functions: &mut Vec<TokenStream>,
) -> (TokenStream, TokenStream) {
    // Don't generate defaults for optional/partial fields
    if is_optional_or_partial {
        return (quote! {}, quote! {});
    }

    // Check for sea_orm(default_value) and sea_orm(primary_key)
    let default_value = extract_sea_orm_default_value(original_attrs);
    let has_pk = has_sea_orm_primary_key(original_attrs);

    // No default source found
    if default_value.is_none() && !has_pk {
        return (quote! {}, quote! {});
    }

    let has_existing_serde_default = extract_default(original_attrs).is_some();

    match &default_value {
        // Literal default (e.g., "42", "draft", "0.7")
        Some(value) if !is_sql_function_default(value) => {
            let schema_default_attr = quote! { #[schema(default = #value)] };

            if has_existing_serde_default {
                return (quote! {}, schema_default_attr);
            }

            if !is_parseable_type(original_ty) {
                return (quote! {}, schema_default_attr);
            }

            let fn_name = format!("default_{struct_name}_{field_name}");
            let fn_ident = syn::Ident::new(&fn_name, proc_macro2::Span::call_site());

            // Validate the literal against the field's type at macro-expansion
            // time: a malformed `default_value` (e.g. `"abc"` on an `i32`, or
            // `"300"` on a `u8`) becomes a COMPILE error pointing at the field
            // instead of the runtime panic the generated `#value.parse().unwrap()`
            // would raise the first time serde fills a missing field.  A valid
            // literal keeps the byte-identical prior `.parse().unwrap()` body, so
            // no currently-valid default changes behaviour.
            let fn_body = match validate_literal_default(value, original_ty) {
                Ok(()) => quote! { #value.parse().unwrap() },
                Err(msg) => syn::Error::new_spanned(original_ty, msg).to_compile_error(),
            };

            default_functions.push(quote! {
                #[allow(non_snake_case)]
                fn #fn_ident() -> #field_ty {
                    #fn_body
                }
            });

            let serde_default_attr = quote! { #[serde(default = #fn_name)] };
            (serde_default_attr, schema_default_attr)
        }
        // SQL function default (NOW(), gen_random_uuid(), etc.) or primary_key auto-increment
        _ => {
            let Some((default_expr, schema_default_str)) =
                sql_function_default_for_type(original_ty)
            else {
                return (quote! {}, quote! {});
            };

            let schema_default_attr = quote! { #[schema(default = #schema_default_str)] };

            if has_existing_serde_default {
                return (quote! {}, schema_default_attr);
            }

            let fn_name = format!("default_{struct_name}_{field_name}");
            let fn_ident = syn::Ident::new(&fn_name, proc_macro2::Span::call_site());

            default_functions.push(quote! {
                #[allow(non_snake_case)]
                fn #fn_ident() -> #field_ty {
                    #default_expr
                }
            });

            let serde_default_attr = quote! { #[serde(default = #fn_name)] };
            (serde_default_attr, schema_default_attr)
        }
    }
}

/// Return a type-appropriate (Rust default expression, OpenAPI default string) pair
/// for fields with SQL function defaults or implicit auto-increment.
///
/// The Rust expression is used in the generated `#[serde(default = "fn")]` function body.
/// The OpenAPI string is used in `#[schema(default = "value")]`.
fn sql_function_default_for_type(original_ty: &syn::Type) -> Option<(TokenStream, String)> {
    let syn::Type::Path(type_path) = original_ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    let type_name = segment.ident.to_string();

    match type_name.as_str() {
        "DateTimeWithTimeZone" | "DateTimeUtc" | "DateTime" => {
            let expr = quote! {
                vespera::chrono::DateTime::<vespera::chrono::Utc>::UNIX_EPOCH.fixed_offset()
            };
            Some((expr, "1970-01-01T00:00:00+00:00".to_string()))
        }
        "NaiveDateTime" => {
            let expr = quote! {
                vespera::chrono::NaiveDateTime::UNIX_EPOCH
            };
            Some((expr, "1970-01-01T00:00:00".to_string()))
        }
        "NaiveDate" => {
            let expr = quote! {
                vespera::chrono::NaiveDate::default()
            };
            Some((expr, "1970-01-01".to_string()))
        }
        "NaiveTime" | "Time" => {
            let expr = quote! {
                vespera::chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
            };
            Some((expr, "00:00:00".to_string()))
        }
        "Uuid" => Some((
            quote! { Default::default() },
            "00000000-0000-0000-0000-000000000000".to_string(),
        )),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" | "f32" | "f64" | "Decimal" => {
            Some((quote! { Default::default() }, "0".to_string()))
        }
        "bool" => Some((quote! { Default::default() }, "false".to_string())),
        "String" => Some((quote! { Default::default() }, String::new())),
        _ => None,
    }
}

/// Check if a type is known to implement `FromStr` and can use `.parse().unwrap()`.
///
/// Returns true for primitive types, String, and Decimal.
/// Returns false for enums and unknown custom types.
pub(super) fn is_parseable_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    let Some(segment) = type_path.path.segments.last() else {
        return false;
    };
    type_utils::PRIMITIVE_TYPE_NAMES.contains(&segment.ident.to_string().as_str())
}

/// Validate a literal `default_value` against the field's type **at
/// macro-expansion time**, mirroring exactly the runtime `#value.parse()`
/// the generated default function performs (no trimming — the generated
/// code does not trim either, so this predicts the runtime result precisely).
///
/// Returns `Err(msg)` when the literal cannot parse to the concrete field
/// type, so the caller emits a `compile_error!` (pointing at the field)
/// instead of generating a `.parse().unwrap()` that panics the first time
/// serde fills a missing field.  Types whose `FromStr` cannot be faithfully
/// reproduced here return `Ok(())`:
/// - `String` — its `FromStr` is infallible.
/// - `Decimal` — needs the `rust_decimal` runtime crate; left to runtime.
/// - any non-primitive / unknown type — already gated out by
///   [`is_parseable_type`] before this is reached.
fn validate_literal_default(value: &str, ty: &syn::Type) -> Result<(), String> {
    let syn::Type::Path(type_path) = ty else {
        return Ok(());
    };
    let Some(segment) = type_path.path.segments.last() else {
        return Ok(());
    };
    let type_name = segment.ident.to_string();

    // Parse against the EXACT field type so a range violation (e.g. `"300"`
    // on a `u8`) is caught, not just a syntactic one.  The message carries
    // the offending value and type plus the underlying `FromStr` error — the
    // same error the runtime `.unwrap()` would have panicked with.
    macro_rules! check {
        ($t:ty) => {
            value
                .parse::<$t>()
                .map(|_| ())
                .map_err(|e| format!("invalid default_value {value:?} for `{type_name}`: {e}"))
        };
    }

    match type_name.as_str() {
        "i8" => check!(i8),
        "i16" => check!(i16),
        "i32" => check!(i32),
        "i64" => check!(i64),
        "i128" => check!(i128),
        "isize" => check!(isize),
        "u8" => check!(u8),
        "u16" => check!(u16),
        "u32" => check!(u32),
        "u64" => check!(u64),
        "u128" => check!(u128),
        "usize" => check!(usize),
        "f32" => check!(f32),
        "f64" => check!(f64),
        "bool" => check!(bool),
        // `String::FromStr` is infallible; `Decimal` needs the runtime crate.
        // Everything else is gated out by `is_parseable_type` before this call.
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
