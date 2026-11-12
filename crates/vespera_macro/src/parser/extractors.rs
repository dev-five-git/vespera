use syn::{GenericArgument, PathArguments, Type, punctuated::Punctuated, token::Comma};

/// If `ty` is `Validated<Inner>` or `ValidatedWith<Ctx, Inner>`, return
/// `Inner`; otherwise return `ty`.
pub(super) fn unwrap_validated_type(ty: &Type) -> &Type {
    validated_inner_type(ty).unwrap_or(ty)
}

/// Return true when the type is a `Validated<...>` or `ValidatedWith<..., ...>`
/// extractor wrapper.
pub(super) fn is_validated_type(ty: &Type) -> bool {
    validated_inner_type(ty).is_some()
}

/// The extractor wrapped by `Validated<T>` (its first generic argument) or by
/// `ValidatedWith<C, T>` (its LAST generic argument — the first one is the
/// validation context, which never reaches the wire).
pub(super) fn validated_inner_type(ty: &Type) -> Option<&Type> {
    extractor_inner_type(ty, "Validated")
        .or_else(|| as_type(extractor_args(ty, "ValidatedWith")?.last()))
}

/// Payload-carrying extractors that `vespera::Validated` / `ValidatedWith`
/// support out of the box (`ValidatePayloadWith` impls in `vespera`).
#[cfg(feature = "validation")]
const PAYLOAD_EXTRACTORS: [&str; 5] = ["Json", "Form", "Query", "Path", "TypedMultipart"];

/// The validated payload `U` of a `Validated<E<U>>` / `ValidatedWith<C, E<U>>`
/// argument, where `E` is one of the built-in payload extractors. A custom
/// extractor (the user's own `ValidatePayload` impl) yields `None` — its
/// payload type cannot be named syntactically.
#[cfg(feature = "validation")]
pub fn validated_payload_type(ty: &Type) -> Option<&Type> {
    let extractor = validated_inner_type(ty)?;
    PAYLOAD_EXTRACTORS
        .iter()
        .find_map(|name| extractor_inner_type(extractor, name))
}

/// Extract the first generic type argument from an extractor by final path segment.
pub(super) fn extractor_inner_type<'a>(ty: &'a Type, extractor: &str) -> Option<&'a Type> {
    as_type(extractor_args(ty, extractor)?.first())
}

/// The angle-bracketed generic arguments of `ty` when its final path segment
/// is `extractor`.
fn extractor_args<'a>(
    ty: &'a Type,
    extractor: &str,
) -> Option<&'a Punctuated<GenericArgument, Comma>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != extractor {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    Some(&args.args)
}

/// `Some(ty)` when `arg` is a type argument (not a lifetime / const).
const fn as_type(arg: Option<&GenericArgument>) -> Option<&Type> {
    match arg {
        Some(GenericArgument::Type(inner_ty)) => Some(inner_ty),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::validated("vespera::Validated<axum::Json<User>>", "axum :: Json < User >")]
    #[case::validated_with("ValidatedWith<Ctx, Json<User>>", "Json < User >")]
    #[case::validated_with_qualified(
        "vespera::ValidatedWith<crate::Ctx, axum::extract::Query<Filter>>",
        "axum :: extract :: Query < Filter >"
    )]
    fn unwraps_validated_inner_extractor(#[case] src: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(src).unwrap();
        assert!(is_validated_type(&ty));
        let inner = unwrap_validated_type(&ty);
        assert_eq!(quote::quote!(#inner).to_string(), expected);
    }

    #[cfg(feature = "validation")]
    #[rstest]
    #[case::json("Validated<Json<User>>", Some("User"))]
    #[case::form("Validated<axum::Form<User>>", Some("User"))]
    #[case::query("ValidatedWith<Ctx, Query<Filter>>", Some("Filter"))]
    #[case::path("Validated<Path<(i64, String)>>", Some("(i64 , String)"))]
    #[case::multipart("Validated<TypedMultipart<Upload>>", Some("Upload"))]
    #[case::custom_extractor("Validated<MyExtractor<User>>", None)]
    #[case::not_validated("Json<User>", None)]
    fn finds_validated_payload(#[case] src: &str, #[case] expected: Option<&str>) {
        let ty: Type = syn::parse_str(src).unwrap();
        let payload = validated_payload_type(&ty).map(|p| quote::quote!(#p).to_string());
        assert_eq!(payload.as_deref(), expected);
    }

    #[rstest]
    #[case::reference("&str")]
    #[case::bare("Validated")]
    #[case::lifetime("Validated<'static>")]
    #[case::bare_with("ValidatedWith")]
    #[case::with_lifetime_last("ValidatedWith<Ctx, 'static>")]
    #[case::with_empty("ValidatedWith<>")]
    #[case::plain_json("Json<User>")]
    fn rejects_non_validated_shapes(#[case] src: &str) {
        let ty: Type = syn::parse_str(src).unwrap();
        assert!(validated_inner_type(&ty).is_none());
        assert!(!is_validated_type(&ty));
        assert!(std::ptr::eq(unwrap_validated_type(&ty), &raw const ty));
    }
}
