//! `#[route]` check that a `Path<T>` extractor can deserialize the route's
//! path parameters. A mismatch used to compile, document the segments as
//! `string`, and fail on every request (axum's `Path` rejection: `500` for a
//! wrong argument count, `400` for a missing struct field).
//!
//! Checked (error spanned on the `Path<..>` type):
//! - `Path<(A, B, ..)>`: the tuple length must equal the parameter count.
//! - `Path<scalar>` (a primitive such as `u32` / `String`): exactly one
//!   parameter.
//! - `Path<Struct>` (a named-field struct declared in the route's own file):
//!   every field serde requires (not `Option`, no field / container
//!   `default`, not `skip` / `skip_deserializing`) must name a parameter (its
//!   serde name or an `alias`); with `deny_unknown_fields`, every parameter
//!   must name a field. A struct with a `#[serde(flatten)]` field is not
//!   checked, and a parameter without a field is otherwise fine (serde
//!   ignores unknown keys).
//!
//! Anything else (`HashMap`, `Vec`, a type declared elsewhere, generics) is
//! left to axum.

use std::path::Path as FsPath;

use quote::ToTokens;
use syn::Type;

use crate::parser::extract_path_parameters;
use crate::parser::schema::{
    SerdeSide, extract_default, extract_deny_unknown_fields, extract_field_rename_for,
    extract_flatten, extract_rename_all_for, extract_serde_skip_flags, is_primitive_type,
    rename_field, strip_raw_prefix_owned,
};

/// Check every `Path<T>` / `Validated<Path<T>>` argument of `sig` against
/// the `{params}` of the `#[route(path)]` (none without a `path`). Structs
/// are looked up among the top-level items of `source_file`.
pub fn check_path_extractors(
    sig: &syn::Signature,
    route_path: Option<&str>,
    source_file: Option<&FsPath>,
) -> syn::Result<()> {
    let lookup = |name: &str| {
        let file = source_file?;
        let definition = crate::schema_macro::file_cache::get_struct_definition(file, name)?;
        syn::parse_str::<syn::ItemStruct>(&definition).ok()
    };
    check_with_lookup(sig, route_path, lookup)
}

pub(super) fn check_with_lookup(
    sig: &syn::Signature,
    route_path: Option<&str>,
    lookup: impl Fn(&str) -> Option<syn::ItemStruct>,
) -> syn::Result<()> {
    let params = extract_path_parameters(route_path.unwrap_or(""));
    let route_path =
        route_path.map_or_else(|| "without `path`".to_string(), |path| format!("`{path}`"));
    let route_path = route_path.as_str();
    for input in &sig.inputs {
        let syn::FnArg::Typed(pat_type) = input else {
            continue;
        };
        let ty = unwrap_extractor(&pat_type.ty, "Validated").unwrap_or(&pat_type.ty);
        let Some(inner) = unwrap_extractor(ty, "Path") else {
            continue;
        };
        if let Some(message) = mismatch(inner, &params, route_path, &lookup) {
            return Err(syn::Error::new_spanned(ty, message));
        }
    }
    Ok(())
}

/// The first type argument of `ty` when its last path segment is `name`.
fn unwrap_extractor<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first()? {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn mismatch(
    inner: &Type,
    params: &[String],
    route_path: &str,
    lookup: &impl Fn(&str) -> Option<syn::ItemStruct>,
) -> Option<String> {
    let extracts = |count: usize| {
        let noun = if count == 1 { "" } else { "s" };
        let route_noun = if params.len() == 1 { "" } else { "s" };
        format!(
            "route {route_path} has {} path parameter{route_noun} but `Path<{}>` extracts {count} value{noun}",
            params.len(),
            type_display(inner),
        )
    };
    match inner {
        Type::Tuple(tuple) => {
            (tuple.elems.len() != params.len()).then(|| extracts(tuple.elems.len()))
        }
        Type::Paren(paren) => mismatch(&paren.elem, params, route_path, lookup),
        _ if is_primitive_type(inner) => (params.len() != 1).then(|| extracts(1)),
        Type::Path(type_path)
            if type_path.qself.is_none() && type_path.path.segments.len() == 1 =>
        {
            let segment = &type_path.path.segments[0];
            if !segment.arguments.is_empty() {
                return None;
            }
            let item = lookup(&segment.ident.to_string())?;
            struct_mismatch(&item, inner, params, route_path)
        }
        _ => None,
    }
}

fn struct_mismatch(
    item: &syn::ItemStruct,
    inner: &Type,
    params: &[String],
    route_path: &str,
) -> Option<String> {
    let syn::Fields::Named(fields) = &item.fields else {
        return None;
    };
    if !item.generics.params.is_empty() || fields.named.iter().any(|f| extract_flatten(&f.attrs)) {
        return None;
    }
    let rename_all = extract_rename_all_for(&item.attrs, SerdeSide::Deserialize);
    let container_default = extract_default(&item.attrs).is_some();
    let mut matched = vec![false; params.len()];
    for field in &fields.named {
        let skip = extract_serde_skip_flags(&field.attrs);
        if skip.skip || skip.skip_deserializing {
            continue;
        }
        let names = field_wire_names(field, rename_all.as_deref());
        let mut found = false;
        for (seen, param) in matched.iter_mut().zip(params) {
            if names.contains(param) {
                *seen = true;
                found = true;
            }
        }
        let optional =
            container_default || extract_default(&field.attrs).is_some() || is_option(&field.ty);
        if !found && !optional {
            let rust_name = field
                .ident
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            return Some(format!(
                "route {route_path} has no path parameter `{{{wire}}}` for the required field `{rust_name}` of `Path<{ty}>`; add `{{{wire}}}` to the route path, or make the field `Option<_>` / `#[serde(default)]`",
                wire = names[0],
                ty = type_display(inner),
            ));
        }
    }
    if extract_deny_unknown_fields(&item.attrs)
        && let Some((param, _)) = params.iter().zip(&matched).find(|(_, seen)| !**seen)
    {
        return Some(format!(
            "route {route_path} has the path parameter `{{{param}}}`, but `Path<{}>` has no field for it and denies unknown fields",
            type_display(inner),
        ));
    }
    None
}

/// The names serde reads a field from: its (deserialize-side) wire name
/// first, then every `alias`.
fn field_wire_names(field: &syn::Field, rename_all: Option<&str>) -> Vec<String> {
    let primary =
        extract_field_rename_for(&field.attrs, SerdeSide::Deserialize).unwrap_or_else(|| {
            let rust_name = field.ident.as_ref().map_or_else(String::new, |ident| {
                strip_raw_prefix_owned(ident.to_string())
            });
            rename_field(&rust_name, rename_all)
        });
    let mut names = vec![primary];
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
    {
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("alias") {
                names.push(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse::<syn::Expr>()?;
            } else if meta.input.peek(syn::token::Paren) {
                meta.input.parse::<proc_macro2::Group>()?;
            }
            Ok(())
        });
    }
    names
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

/// `quote` spacing (`(u32 , String)`) tidied for a diagnostic.
fn type_display(ty: &Type) -> String {
    ty.to_token_stream()
        .to_string()
        .replace(" ,", ",")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" :: ", "::")
        .replace(":: ", "::")
        .replace(" < ", "<")
        .replace("< ", "<")
        .replace(" <", "<")
        .replace(" >", ">")
        .replace("& ", "&")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const STRUCTS: &str = r#"
        struct Ids { org: String, id: u32 }
        struct Renamed { #[serde(rename = "userId")] user_id: u32 }
        #[serde(rename_all = "camelCase")]
        struct Camel { user_id: u32 }
        struct Aliased { #[serde(alias = "uid", default = "f")] id: u32, #[serde(alias = "o")] org: String }
        struct Optional { id: u32, page: Option<u32>, #[serde(default)] size: u32, #[serde(skip)] cache: u8 }
        #[serde(default)]
        struct AllDefault { id: u32, other: u32 }
        struct Flat { #[serde(flatten)] rest: Ids }
        #[serde(deny_unknown_fields)]
        struct Strict { id: u32 }
        struct Generic<T> { id: T }
    "#;

    fn lookup(name: &str) -> Option<syn::ItemStruct> {
        let file: syn::File = syn::parse_str(STRUCTS).unwrap();
        file.items.into_iter().find_map(|item| match item {
            syn::Item::Struct(s) if s.ident == name => Some(s),
            _ => None,
        })
    }

    fn check(path: Option<&str>, arg: &str) -> Result<(), String> {
        let sig: syn::Signature = syn::parse_str(&format!("async fn h({arg})")).unwrap();
        check_with_lookup(&sig, path, lookup).map_err(|e| e.to_string())
    }

    #[rstest]
    #[case::scalar_one(Some("/{id}"), "Path(id): Path<u32>")]
    #[case::qualified(Some("/{id}"), "p: axum::extract::Path<String>")]
    #[case::tuple_exact(Some("/{a}/{b}"), "Path((a, b)): Path<(String, u32)>")]
    #[case::one_tuple(Some("/{a}"), "p: Path<(u32,)>")]
    #[case::validated_tuple(Some("/{a}/{b}"), "p: Validated<Path<(String, u32)>>")]
    #[case::axum_07(Some("/:a/{*b}"), "p: Path<(String, String)>")]
    #[case::map(Some("/{a}/{b}"), "p: Path<HashMap<String, String>>")]
    #[case::vec(Some("/{a}/{b}"), "p: Path<Vec<(String, String)>>")]
    #[case::unknown_type(Some("/{a}/{b}"), "p: Path<Elsewhere>")]
    #[case::qualified_struct(Some("/{a}"), "p: Path<other::Ids>")]
    #[case::struct_exact(Some("/{org}/{id}"), "p: Path<Ids>")]
    #[case::struct_extra_param(Some("/{org}/{id}/{x}"), "p: Path<Ids>")]
    #[case::struct_renamed(Some("/{userId}"), "p: Path<Renamed>")]
    #[case::struct_rename_all(Some("/{userId}"), "p: Path<Camel>")]
    #[case::struct_alias(Some("/{o}"), "p: Path<Aliased>")]
    #[case::struct_optional(Some("/{id}"), "p: Path<Optional>")]
    #[case::container_default(Some("/{x}"), "p: Path<AllDefault>")]
    #[case::flatten(Some("/{x}"), "p: Path<Flat>")]
    #[case::strict_exact(Some("/{id}"), "p: Path<Strict>")]
    #[case::generic(Some("/{x}"), "p: Path<Generic<u32>>")]
    #[case::no_path_extractor(None, "Json(b): Json<(u32, u32)>")]
    fn accepted(#[case] path: Option<&str>, #[case] arg: &str) {
        assert_eq!(check(path, arg), Ok(()));
    }

    #[rstest]
    #[case::tuple_short(
        Some("/a/{x}/{y}/{z}"),
        "Path(p): Path<(A, B)>",
        "route `/a/{x}/{y}/{z}` has 3 path parameters but `Path<(A, B)>` extracts 2 values"
    )]
    #[case::tuple_long(
        Some("/{x}"),
        "p: Path<(u32, String)>",
        "route `/{x}` has 1 path parameter but `Path<(u32, String)>` extracts 2 values"
    )]
    #[case::validated_tuple(
        Some("/{x}"),
        "p: Validated<Path<(u32, u32)>>",
        "route `/{x}` has 1 path parameter but `Path<(u32, u32)>` extracts 2 values"
    )]
    #[case::scalar_two(
        Some("/{a}/{b}"),
        "p: axum::extract::Path<u32>",
        "route `/{a}/{b}` has 2 path parameters but `Path<u32>` extracts 1 value"
    )]
    #[case::scalar_none(
        None,
        "p: Path<String>",
        "route without `path` has 0 path parameters but `Path<String>` extracts 1 value"
    )]
    #[case::struct_missing_field(
        Some("/{org}"),
        "p: Path<Ids>",
        "route `/{org}` has no path parameter `{id}` for the required field `id` of `Path<Ids>`; add `{id}` to the route path, or make the field `Option<_>` / `#[serde(default)]`"
    )]
    #[case::struct_rename_all_missing(
        Some("/{user_id}"),
        "p: Path<Camel>",
        "route `/{user_id}` has no path parameter `{userId}` for the required field `user_id` of `Path<Camel>`; add `{userId}` to the route path, or make the field `Option<_>` / `#[serde(default)]`"
    )]
    #[case::strict_extra(
        Some("/{id}/{x}"),
        "p: Path<Strict>",
        "route `/{id}/{x}` has the path parameter `{x}`, but `Path<Strict>` has no field for it and denies unknown fields"
    )]
    fn rejected(#[case] path: Option<&str>, #[case] arg: &str, #[case] message: &str) {
        assert_eq!(check(path, arg), Err(message.to_string()));
    }
}
