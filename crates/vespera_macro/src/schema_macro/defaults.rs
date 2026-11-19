//! SeaORM default-value attribute generation.
//!
//! Translates `#[sea_orm(default_value = ...)]` / `#[sea_orm(primary_key)]`
//! on source fields into `#[serde(default = "...")]` + `#[schema(default = "...")]`
//! attributes (plus companion default functions) on the generated struct.

use proc_macro2::TokenStream;
use quote::quote;

use super::cfg_eval::Predicate;
use super::field_attrs::CarriedAttrs;
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
/// 3. **Primary key** (implicit auto-increment, see [`has_sea_orm_primary_key`]
///    — `primary_key_count` is the model's number of key columns):
///    Treated as having an implicit default — generates type-specific default.
///
/// Skips serde default generation when:
/// - The field is wrapped in `Option` (partial mode or already optional)
/// - The field already has `#[serde(default)]` (then only the bare
///   `#[schema(default)]` marker is emitted: serde applies the field's own
///   default, not the SeaORM one)
/// - For literal defaults: the field type doesn't implement `FromStr`
///
/// The field's defaults with no source module (a bare name is then trusted).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_sea_orm_default_attrs(
    original_attrs: &[syn::Attribute],
    struct_name: &syn::Ident,
    field_name: &str,
    original_ty: &syn::Type,
    field_ty: &dyn quote::ToTokens,
    is_optional_or_partial: bool,
    primary_key_count: usize,
    default_functions: &mut Vec<TokenStream>,
) -> (TokenStream, TokenStream) {
    generate_sea_orm_default_attrs_in(
        original_attrs,
        struct_name,
        field_name,
        original_ty,
        field_ty,
        is_optional_or_partial,
        primary_key_count,
        &[],
        default_functions,
    )
}

/// [`generate_sea_orm_default_attrs`] for a field of the source module
/// `source_module`: a bare `Date` / `Time` / `Uuid` is judged there
/// ([`default_alias_is_external`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_sea_orm_default_attrs_in(
    original_attrs: &[syn::Attribute],
    struct_name: &syn::Ident,
    field_name: &str,
    original_ty: &syn::Type,
    field_ty: &dyn quote::ToTokens,
    is_optional_or_partial: bool,
    primary_key_count: usize,
    source_module: &[String],
    default_functions: &mut Vec<TokenStream>,
) -> (TokenStream, TokenStream) {
    // Don't generate defaults for optional/partial fields
    if is_optional_or_partial {
        return (quote! {}, quote! {});
    }

    // Check for sea_orm(default_value) and sea_orm(primary_key)
    let default_value = extract_sea_orm_default_value(original_attrs);
    let has_pk = has_sea_orm_primary_key(original_attrs, original_ty, primary_key_count);

    // No default source found
    if default_value.is_none() && !has_pk {
        return (quote! {}, quote! {});
    }

    // The field's own `#[serde(default)]` / `default = "fn"` is what serde
    // applies, so the SeaORM value must not be advertised: the bare
    // `#[schema(default)]` marker lets the DTO's `#[derive(Schema)]` document
    // the serde default instead (or none, when it cannot be resolved).
    if extract_default(original_attrs).is_some() {
        return (quote! {}, quote! { #[schema(default)] });
    }

    match &default_value {
        // Literal default (e.g., "42", "draft", "0.7")
        Some(value) if !is_sql_function_default(value) => {
            let schema_default_attr = quote! { #[schema(default = #value)] };

            // No serde default can be generated for a non-primitive type, so
            // the literal is not advertised either: an OpenAPI default for a
            // field serde still requires (a missing key is a 422) was a lie
            // (R20-L1, round 20).
            if !is_parseable_type(original_ty) {
                return (quote! {}, quote! {});
            }

            let fn_name = format!("default_{}_{field_name}", unraw(struct_name));
            let fn_ident = syn::Ident::new(&fn_name, proc_macro2::Span::call_site());

            // Validate the literal against the field's type at macro-expansion
            // time: a malformed `default_value` (e.g. `"abc"` on an `i32`, or
            // `"300"` on a `u8`) becomes a COMPILE error pointing at the field
            // instead of the runtime panic the generated `#value.parse().unwrap()`
            // would raise the first time serde fills a missing field.  A valid
            // literal keeps the byte-identical prior `.parse().unwrap()` body, so
            // no currently-valid default changes behaviour.
            let fn_body = match validate_literal_default(value, original_ty) {
                Ok(()) => {
                    let guard = pointer_width_guard(value, original_ty);
                    quote! { #guard #value.parse().unwrap() }
                }
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
                    .filter(|_| {
                        matches!(original_ty, syn::Type::Path(path) if default_alias_is_external(path, source_module))
                    })
            else {
                return (quote! {}, quote! {});
            };

            // A machine-dependent value (`Local`) advertises no OpenAPI
            // default, but the bare `#[schema(default)]` marker still tells
            // the DTO's `#[derive(Schema)]` the serde default is deliberate
            // (its generated fn is not in any source file to resolve).
            let schema_default_attr = schema_default_str.map_or_else(
                || quote! { #[schema(default)] },
                |value| quote! { #[schema(default = #value)] },
            );

            let fn_name = format!("default_{}_{field_name}", unraw(struct_name));
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

/// A field's generated default fns, `#[allow(dead_code)]` where its serde
/// default is `cfg_attr`-gated: in the build where that override applies,
/// nothing references the fn (R30-SMB1-L1), and the consumer crate would warn.
pub(super) fn allow_unused_default_fns(
    functions: Vec<TokenStream>,
    carried: &CarriedAttrs,
) -> Vec<TokenStream> {
    if carried.cfg_serde_default_predicates().is_empty() {
        return functions;
    }
    functions
        .into_iter()
        .map(|function| quote! { #[allow(dead_code)] #function })
        .collect()
}

/// Gates the SeaORM-derived `(serde, schema)` defaults on the field's
/// `cfg_attr` overrides. Where a `cfg_attr(P, serde(default ..))` holds,
/// serde applies the field's own default (the generated `default = ".."`
/// would be a second `default`, which serde rejects) and the bare
/// `#[schema(default)]` marker documents it; where a `cfg_attr(Q,
/// schema(default ..))` holds, that one documents the field. Without a
/// `Schema` derive (`schema_derived` false) the schema attribute would go to
/// the metadata, which reads no `cfg_attr`, so the SeaORM value could be
/// advertised while a cfg-gated serde default applies: it is dropped instead.
pub(super) fn gate_sea_orm_defaults(
    (serde_attr, schema_attr): (TokenStream, TokenStream),
    carried: &CarriedAttrs,
    schema_derived: bool,
) -> (TokenStream, TokenStream) {
    let serde_cfgs = carried.cfg_serde_default_predicates();
    if serde_cfgs.is_empty() || (serde_attr.is_empty() && schema_attr.is_empty()) {
        return (
            serde_attr,
            gate_attrs(schema_attr, &carried.schema_default_cfgs),
        );
    }
    let serde_attr = gate_attrs(serde_attr, &serde_cfgs);
    if !schema_derived {
        return (serde_attr, TokenStream::new());
    }
    let mut schema = gate_attrs(schema_attr, &serde_cfgs);
    schema.extend(quote! { #[cfg_attr(any(#(#serde_cfgs),*), schema(default))] });
    (serde_attr, gate_attrs(schema, &carried.schema_default_cfgs))
}

/// `#[a] #[b]` -> `#[cfg_attr(not(any(P, ..)), a)] ..`: the attributes
/// apply only where none of `predicates` holds (unchanged without any).
fn gate_attrs(attrs: TokenStream, predicates: &[Predicate]) -> TokenStream {
    if predicates.is_empty() {
        return attrs;
    }
    let attrs = syn::parse::Parser::parse2(syn::Attribute::parse_outer, attrs).unwrap_or_default();
    attrs
        .iter()
        .map(|attr| {
            let meta = &attr.meta;
            quote! { #[cfg_attr(not(any(#(#predicates),*)), #meta)] }
        })
        .collect()
}

/// `ident` without a raw `r#` prefix (it goes into generated fn names).
fn unraw(ident: &syn::Ident) -> String {
    crate::parser::strip_raw_prefix_owned(ident.to_string())
}

/// The SQL-default type names whose default expression is a chrono or uuid
/// value (see [`sql_function_default_for_type`]); a user type of one of these
/// names must not get that expression.
const CHRONO_OR_UUID_DEFAULT_NAMES: &[&str] = &[
    "DateTimeWithTimeZone",
    "DateTimeUtc",
    "DateTimeLocal",
    "DateTime",
    "NaiveDateTime",
    "NaiveDate",
    "Date",
    "NaiveTime",
    "Time",
    "Uuid",
];

/// Whether a SQL-default type named like a chrono / uuid type (`Date`,
/// `DateTime`, `Uuid`, ...) is the chrono / uuid / SeaORM type the default
/// expression is written for, rather than one of the source module's own
/// items (`enum Time`): the expression would not type-check there
/// (R24-SM1-L2, R25-SM1-L2). The path is judged through the module's scope
/// (R25-SM1-L1: `prelude::Date` after `use sea_orm::prelude;`); a bare name
/// the scope cannot pin down (a glob or prelude name) is trusted, as every
/// other default name is. A multi-segment path with an unknown root is
/// judged by its literal root, as before.
pub(super) fn default_alias_is_external(type_path: &syn::TypePath, module: &[String]) -> bool {
    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let name = segments.last().cloned().unwrap_or_default();
    if !CHRONO_OR_UUID_DEFAULT_NAMES.contains(&name.as_str()) {
        return true;
    }
    let external = |path: &[String]| {
        matches!(
            path.first().map(String::as_str),
            Some("chrono" | "uuid" | "sea_orm")
        )
    };
    match crate::schema_macro::module_scope::resolve_name(&segments, module) {
        crate::schema_macro::module_scope::Resolved::Path(path) => external(&path),
        crate::schema_macro::module_scope::Resolved::Unknown if segments.len() > 1 => {
            external(&segments)
        }
        crate::schema_macro::module_scope::Resolved::Unknown => true,
    }
}

/// Return a type-appropriate (Rust default expression, OpenAPI default string) pair
/// for fields with SQL function defaults or implicit auto-increment.
///
/// The Rust expression is used in the generated `#[serde(default = "fn")]` function body.
/// The OpenAPI string is used in `#[schema(default = "value")]`; `None` when the
/// value depends on the machine (a `Local` epoch carries the server's offset).
fn sql_function_default_for_type(original_ty: &syn::Type) -> Option<(TokenStream, Option<String>)> {
    let syn::Type::Path(type_path) = original_ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    let type_name = segment.ident.to_string();
    // `chrono::DateTime<Tz>` keeps its zone; SeaORM's bare `DateTime` alias
    // is the naive one.
    let zone = if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(syn::Type::Path(zone))) = args.args.first()
    {
        zone.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    };
    let epoch = quote! { vespera::chrono::DateTime::<vespera::chrono::Utc>::UNIX_EPOCH };

    match (type_name.as_str(), zone.as_deref()) {
        ("DateTimeWithTimeZone", _) | ("DateTime", Some("FixedOffset")) => Some((
            quote! { #epoch.fixed_offset() },
            Some("1970-01-01T00:00:00+00:00".to_string()),
        )),
        ("DateTimeUtc", _) | ("DateTime", Some("Utc")) => {
            Some((epoch, Some("1970-01-01T00:00:00Z".to_string())))
        }
        ("DateTimeLocal", _) | ("DateTime", Some("Local")) => Some((
            quote! { #epoch.with_timezone(&vespera::chrono::Local) },
            None,
        )),
        ("DateTime" | "NaiveDateTime", None) => Some((
            quote! { vespera::chrono::NaiveDateTime::UNIX_EPOCH },
            Some("1970-01-01T00:00:00".to_string()),
        )),
        ("NaiveDate" | "Date", _) => Some((
            quote! { vespera::chrono::NaiveDate::default() },
            Some("1970-01-01".to_string()),
        )),
        ("NaiveTime" | "Time", _) => Some((
            quote! { vespera::chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap() },
            Some("00:00:00".to_string()),
        )),
        ("Uuid", _) => Some((
            quote! { Default::default() },
            Some("00000000-0000-0000-0000-000000000000".to_string()),
        )),
        (
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
            | "usize" | "f32" | "f64" | "Decimal",
            _,
        ) => Some((quote! { Default::default() }, Some("0".to_string()))),
        ("bool", _) => Some((quote! { Default::default() }, Some("false".to_string()))),
        ("String", _) => Some((quote! { Default::default() }, Some(String::new()))),
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

/// `isize` / `usize` literals are checked at the proc-macro HOST's pointer
/// width; one outside the 16-bit range (the narrowest Rust target) also gets
/// a `const` assertion evaluated at the TARGET's width, so a `wasm32` build
/// of `"5000000000"` on a `usize` fails to compile instead of panicking in
/// the generated `.parse().unwrap()`. Empty for every other literal / type.
fn pointer_width_guard(value: &str, ty: &syn::Type) -> TokenStream {
    let syn::Type::Path(type_path) = ty else {
        return TokenStream::new();
    };
    let name = type_path.path.segments.last().map(|s| s.ident.to_string());
    let check = match name.as_deref() {
        Some("usize") => value
            .parse::<u128>()
            .ok()
            .filter(|n| u16::try_from(*n).is_err())
            .map(|n| {
                let lit = proc_macro2::Literal::u128_suffixed(n);
                quote! { #lit <= ::core::primitive::usize::MAX as u128 }
            }),
        Some("isize") => value
            .parse::<i128>()
            .ok()
            .filter(|n| i16::try_from(*n).is_err())
            .map(|n| {
                let lit = proc_macro2::Literal::i128_suffixed(n.abs());
                let lit = if n < 0 { quote!(-#lit) } else { quote!(#lit) };
                let (min, max) = (
                    quote!(::core::primitive::isize::MIN),
                    quote!(::core::primitive::isize::MAX),
                );
                quote! { #lit >= #min as i128 && #lit <= #max as i128 }
            }),
        _ => None,
    };
    check.map_or_else(TokenStream::new, |check| {
        let message = format!(
            "invalid default_value {value:?}: out of range for the target's `{}`",
            name.unwrap_or_default()
        );
        quote! { const _: () = ::core::assert!(#check, #message); }
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_alias;
#[cfg(test)]
mod tests_allow;
