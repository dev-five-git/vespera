//! `#[route]` check that a `Path<T>` extractor can deserialize the route's
//! path parameters. A definite mismatch used to compile, document the
//! segments as `string`, and fail on every request (axum's `Path`
//! rejection: `500` for a wrong argument count, `400` for an unknown field).
//!
//! The app may be mounted under a parameterised prefix
//! (`Router::nest("/{tenant}", app)`), so the extractor can legitimately see
//! MORE parameters than the route's own `path` declares, never fewer. Only
//! cases that fail whatever the prefix are rejected (error spanned on the
//! `Path<..>` type; `Validated` / `ValidatedWith` / `Result<_, Rejection>`
//! wrappers are seen through):
//! - `Path<(A, B, ..)>` with FEWER elements than the route's own parameters;
//! - `Path<scalar>` (a primitive such as `u32` / `String`) on a route with
//!   two or more own parameters;
//! - `Path<Struct>` (a named-field struct declared in the route's own file)
//!   with `#[serde(deny_unknown_fields)]` when one of the route's own
//!   parameters matches no field (by serde name or `alias`). Structs with a
//!   `#[serde(flatten)]` field or generics are not checked.
//!
//! A longer tuple, a scalar on a route with 0 / 1 own parameters, and a
//! struct field without a matching segment are accepted (a prefix may supply
//! the rest); anything else (`HashMap`, `Vec`, a type declared elsewhere) is
//! left to axum.

use std::path::Path as FsPath;

use quote::ToTokens;
use syn::Type;

use crate::parser::schema::{
    SerdeSide, extract_deny_unknown_fields, extract_field_rename_for, extract_flatten,
    extract_rename_all_for, extract_serde_skip_flags, is_primitive_type, rename_field,
    strip_raw_prefix_owned,
};
use crate::parser::{extract_path_parameters, request_extractor};

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
        let ty = request_extractor(&pat_type.ty);
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
            (tuple.elems.len() < params.len()).then(|| extracts(tuple.elems.len()))
        }
        Type::Paren(paren) => mismatch(&paren.elem, params, route_path, lookup),
        _ if is_primitive_type(inner) => (params.len() >= 2).then(|| extracts(1)),
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
    if !extract_deny_unknown_fields(&item.attrs)
        || !item.generics.params.is_empty()
        || fields.named.iter().any(|f| extract_flatten(&f.attrs))
    {
        return None;
    }
    let rename_all = extract_rename_all_for(&item.attrs, SerdeSide::Deserialize);
    let mut matched = vec![false; params.len()];
    for field in &fields.named {
        let skip = extract_serde_skip_flags(&field.attrs);
        if skip.skip || skip.skip_deserializing {
            continue;
        }
        let names = field_wire_names(field, rename_all.as_deref());
        for (seen, param) in matched.iter_mut().zip(params) {
            *seen |= names.contains(param);
        }
    }
    if let Some((param, _)) = params.iter().zip(&matched).find(|(_, seen)| !**seen) {
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
        #[serde(deny_unknown_fields)]
        struct StrictAliased {
            #[serde(alias = "uid", default = "f")] id: u32,
            #[serde(rename(deserialize = "organization"), alias = "o")] org: String,
            #[serde(default)] page: u32,
        }
        #[serde(deny_unknown_fields)]
        struct StrictTuple(u32);
        #[serde(deny_unknown_fields)]
        struct StrictSkip { id: u32, #[serde(skip)] x: u8 }
        #[serde(deny_unknown_fields)]
        struct StrictFlat { #[serde(flatten)] rest: Ids }
        #[serde(deny_unknown_fields)]
        struct Strict { id: u32 }
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct StrictCamel { user_id: u32 }
        #[serde(deny_unknown_fields)]
        struct StrictGeneric<T> { id: T }
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
    // A nest prefix (`/{tenant}`) may supply MORE parameters than the
    // route's own path declares.
    #[case::tuple_longer_nest_prefix(Some("/{id}"), "Path((t, id)): Path<(String, u32)>")]
    #[case::tuple_without_path(None, "p: Path<(String,)>")]
    #[case::scalar_without_path(None, "p: Path<String>")]
    #[case::struct_exact(Some("/{org}/{id}"), "p: Path<Ids>")]
    #[case::struct_extra_param(Some("/{org}/{id}/{x}"), "p: Path<Ids>")]
    #[case::struct_field_from_prefix(Some("/{id}"), "p: Path<Ids>")]
    #[case::struct_renamed(Some("/{userId}"), "p: Path<Renamed>")]
    #[case::struct_rename_all_mismatch_lenient(Some("/{user_id}"), "p: Path<Camel>")]
    #[case::strict_exact(Some("/{id}"), "p: Path<Strict>")]
    #[case::strict_field_from_prefix(None, "p: Path<Strict>")]
    #[case::strict_alias(Some("/{uid}/{o}"), "p: Path<StrictAliased>")]
    #[case::strict_rename_all(Some("/{userId}"), "p: Path<StrictCamel>")]
    #[case::strict_flatten(Some("/{x}"), "p: Path<StrictFlat>")]
    #[case::strict_generic(Some("/{x}"), "p: Path<StrictGeneric<u32>>")]
    #[case::strict_generic_inferred(Some("/{x}"), "p: Path<StrictGeneric>")]
    #[case::strict_tuple_struct(Some("/{x}"), "p: Path<StrictTuple>")]
    #[case::paren_tuple(Some("/{a}/{b}"), "p: Path<((u32, u32))>")]
    #[case::lifetime_argument(Some("/{a}/{b}"), "p: Path<'static>")]
    #[case::receiver(Some("/{a}/{b}"), "&self, p: Path<(u32, u32)>")]
    #[case::reference_arg(Some("/{a}/{b}"), "r: &str, p: Path<(u32, u32)>")]
    #[case::bare_path_type(Some("/{a}/{b}"), "p: Path")]
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
    #[case::validated_tuple(
        Some("/{x}/{y}"),
        "p: Validated<Path<(u32,)>>",
        "route `/{x}/{y}` has 2 path parameters but `Path<(u32,)>` extracts 1 value"
    )]
    #[case::scalar_two(
        Some("/{a}/{b}"),
        "p: axum::extract::Path<u32>",
        "route `/{a}/{b}` has 2 path parameters but `Path<u32>` extracts 1 value"
    )]
    #[case::strict_rename_all_mismatch(
        Some("/{user_id}"),
        "p: Path<StrictCamel>",
        "route `/{user_id}` has the path parameter `{user_id}`, but `Path<StrictCamel>` has no field for it and denies unknown fields"
    )]
    #[case::strict_skipped_field(
        Some("/{id}/{x}"),
        "p: Path<StrictSkip>",
        "route `/{id}/{x}` has the path parameter `{x}`, but `Path<StrictSkip>` has no field for it and denies unknown fields"
    )]
    #[case::paren_tuple_short(
        Some("/{a}/{b}/{c}"),
        "p: Path<((u32, u32))>",
        "route `/{a}/{b}/{c}` has 3 path parameters but `Path<(u32, u32)>` extracts 2 values"
    )]
    #[case::strict_extra(
        Some("/{id}/{x}"),
        "p: Path<Strict>",
        "route `/{id}/{x}` has the path parameter `{x}`, but `Path<Strict>` has no field for it and denies unknown fields"
    )]
    #[case::result_tuple_short(
        Some("/{x}/{y}"),
        "p: Result<Path<(u32,)>, PathRejection>",
        "route `/{x}/{y}` has 2 path parameters but `Path<(u32,)>` extracts 1 value"
    )]
    #[case::validated_with_tuple_short(
        Some("/{x}/{y}"),
        "p: ValidatedWith<Ctx, Path<(u32,)>>",
        "route `/{x}/{y}` has 2 path parameters but `Path<(u32,)>` extracts 1 value"
    )]
    fn rejected(#[case] path: Option<&str>, #[case] arg: &str, #[case] message: &str) {
        assert_eq!(check(path, arg), Err(message.to_string()));
    }

    #[test]
    fn structs_are_looked_up_in_the_route_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("route.rs");
        std::fs::write(
            &file,
            "#[serde(deny_unknown_fields)] struct Strict { id: u32 }",
        )
        .unwrap();
        let sig: syn::Signature = syn::parse_str("async fn h(p: Path<Strict>)").unwrap();
        let err = check_path_extractors(&sig, Some("/{id}/{x}"), Some(&file))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has the path parameter `{x}`"), "{err}");
        assert!(check_path_extractors(&sig, Some("/{id}"), Some(&file)).is_ok());
        // Unknown in the file / no source file: not checked.
        let other: syn::Signature = syn::parse_str("async fn h(p: Path<Other>)").unwrap();
        assert!(check_path_extractors(&other, Some("/{id}/{x}"), Some(&file)).is_ok());
        assert!(check_path_extractors(&sig, Some("/{id}/{x}"), None).is_ok());
    }
}
